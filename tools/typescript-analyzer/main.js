const fs = require("fs");
const path = require("path");

function main() {
  const input = fs.readFileSync(0, "utf8");
  const request = JSON.parse(input);
  const changedFiles = (request.changed_files || []).filter(isTypeScriptFile);
  const moduleGraph = buildModuleGraph(request.workspace_root);
  const dirs = [...new Set(changedFiles.map((file) => normalizeDir(path.posix.dirname(file))))].sort();

  const response = {
    packages: [],
    files: [],
  };

  for (const dir of dirs) {
    const changedInDir = changedFiles.filter(
      (file) => normalizeDir(path.posix.dirname(file)) === dir,
    );
    const packageSnapshot = analyzeDirectory(request.workspace_root, dir, changedInDir, moduleGraph);
    response.packages.push(packageSnapshot.packageSnapshot);
    response.files.push(...packageSnapshot.fileSnapshots);
  }

  process.stdout.write(JSON.stringify(response));
}

function analyzeDirectory(workspaceRoot, dir, changedFiles, moduleGraph) {
  const exports = {};
  const implementations = [];

  for (const moduleInfo of moduleGraph.modules.values()) {
    if (normalizeDir(path.posix.dirname(moduleInfo.path)) !== dir) {
      continue;
    }

    Object.assign(exports, moduleInfo.exports);

    for (const [className, classInfo] of Object.entries(moduleInfo.classes)) {
      for (const reference of classInfo.implementsRefs) {
        const shape = resolveShape(reference, moduleInfo.path, moduleGraph, new Set());
        if (!shape) {
          continue;
        }
        if (implementsShape(classInfo.members, shape.members)) {
          implementations.push(`${className}=>${shape.name}`);
        }
      }
    }
  }

  implementations.sort();

  const fileSnapshots = [];
  for (const changedFile of changedFiles) {
    if (isTestFile(changedFile)) {
      continue;
    }
    const moduleInfo = moduleGraph.modules.get(changedFile);
    if (!moduleInfo) {
      continue;
    }
    fileSnapshots.push(snapshotFile(moduleInfo));
  }

  return {
    packageSnapshot: {
      dir,
      exports,
      implementations,
    },
    fileSnapshots,
  };
}

function buildModuleGraph(workspaceRoot) {
  const files = walkFiles(workspaceRoot)
    .map((absolutePath) => normalizePath(path.relative(workspaceRoot, absolutePath)))
    .filter(isTypeScriptFile)
    .filter((file) => !file.includes("/node_modules/"));

  const modules = new Map();

  for (const relativePath of files) {
    const absolutePath = path.join(workspaceRoot, relativePath);
    const source = fs.readFileSync(absolutePath, "utf8");
    const masked = maskSource(source);
    modules.set(relativePath, parseModule(relativePath, source, masked));
  }

  for (const moduleInfo of modules.values()) {
    moduleInfo.imports = resolveImports(moduleInfo.rawImports, moduleInfo.path, modules);
  }

  return { modules };
}

function walkFiles(root) {
  const result = [];
  const stack = [root];

  while (stack.length > 0) {
    const current = stack.pop();
    const entries = fs.readdirSync(current, { withFileTypes: true });
    for (const entry of entries) {
      if (entry.name === ".git" || entry.name === "node_modules") {
        continue;
      }

      const absolutePath = path.join(current, entry.name);
      if (entry.isDirectory()) {
        stack.push(absolutePath);
        continue;
      }
      if (entry.isFile()) {
        result.push(absolutePath);
      }
    }
  }

  return result;
}

function parseModule(modulePath, source, masked) {
  return {
    path: modulePath,
    source,
    masked,
    rawImports: parseImports(source),
    imports: {
      named: new Map(),
      namespace: new Map(),
    },
    exports: collectExportedDeclarations(source, masked),
    interfaces: collectInterfaces(source, masked),
    typeAliases: collectTypeAliases(source, masked),
    classes: collectClasses(source, masked),
  };
}

function parseImports(masked) {
  const imports = [];

  const namedPattern = /import\s+(?:type\s+)?\{([^}]+)\}\s+from\s+["']([^"']+)["']/g;
  let match;
  while ((match = namedPattern.exec(masked)) !== null) {
    const bindings = match[1]
      .split(",")
      .map((entry) => entry.trim())
      .filter(Boolean)
      .map((entry) => {
        const parts = entry.split(/\s+as\s+/);
        return {
          imported: parts[0].trim(),
          local: (parts[1] || parts[0]).trim(),
        };
      });
    imports.push({
      kind: "named",
      source: match[2].trim(),
      bindings,
    });
  }

  const namespacePattern = /import\s+(?:type\s+)?\*\s+as\s+([A-Za-z_$][\w$]*)\s+from\s+["']([^"']+)["']/g;
  while ((match = namespacePattern.exec(masked)) !== null) {
    imports.push({
      kind: "namespace",
      source: match[2].trim(),
      local: match[1].trim(),
    });
  }

  return imports;
}

function resolveImports(rawImports, modulePath, modules) {
  const resolved = {
    named: new Map(),
    namespace: new Map(),
  };

  for (const entry of rawImports) {
    const target = resolveImportPath(modulePath, entry.source, modules);
    if (!target) {
      continue;
    }

    if (entry.kind === "named") {
      for (const binding of entry.bindings) {
        resolved.named.set(binding.local, {
          modulePath: target,
          exported: binding.imported,
        });
      }
      continue;
    }

    if (entry.kind === "namespace") {
      resolved.namespace.set(entry.local, target);
    }
  }

  return resolved;
}

function resolveImportPath(fromModulePath, specifier, modules) {
  if (!specifier.startsWith(".")) {
    return null;
  }

  const baseDir = path.posix.dirname(fromModulePath);
  const raw = normalizePath(path.posix.join(baseDir, specifier));
  const candidates = [
    raw,
    `${raw}.ts`,
    `${raw}.tsx`,
    `${raw}/index.ts`,
    `${raw}/index.tsx`,
  ];

  for (const candidate of candidates) {
    if (modules.has(candidate)) {
      return candidate;
    }
  }

  return null;
}

function collectExportedDeclarations(source, masked) {
  const exports = {};
  const declarations = [
    /export\s+(?:default\s+)?(?:async\s+)?function\s+([A-Za-z_$][\w$]*)\s*\(/g,
    /export\s+(?:default\s+)?class\s+([A-Za-z_$][\w$]*)[^{]*\{/g,
    /export\s+interface\s+([A-Za-z_$][\w$]*)[^{]*\{/g,
    /export\s+type\s+([A-Za-z_$][\w$]*)\s*=/g,
  ];

  for (const pattern of declarations) {
    pattern.lastIndex = 0;
    let match;
    while ((match = pattern.exec(masked)) !== null) {
      const [statement, name] = match;
      const start = match.index;
      if (statement.includes("{")) {
        const openIndex = masked.indexOf("{", start);
        if (openIndex === -1) {
          continue;
        }
        const closingIndex = findMatching(masked, openIndex, "{", "}");
        if (closingIndex === -1) {
          continue;
        }
        const slice =
          statement.includes("class ") || statement.includes("function ")
            ? source.slice(start, openIndex).trim()
            : source.slice(start, closingIndex + 1).trim();
        exports[name] = normalizeSignature(slice);
        pattern.lastIndex = Math.max(pattern.lastIndex, closingIndex + 1);
        continue;
      }

      const end = findStatementEnd(masked, start + statement.length);
      exports[name] = normalizeSignature(source.slice(start, end).trim());
      pattern.lastIndex = Math.max(pattern.lastIndex, end);
    }
  }

  const constPattern = /export\s+(?:const|let|var)\s+([A-Za-z_$][\w$]*)\s*[:=]/g;
  let match;
  while ((match = constPattern.exec(masked)) !== null) {
    const [statement, name] = match;
    const start = match.index;
    const end = findStatementEnd(masked, start + statement.length);
    const declaration = normalizeSignature(source.slice(start, end).trim());
    if (!shouldTrackExportedConstDeclaration(declaration)) {
      constPattern.lastIndex = Math.max(constPattern.lastIndex, end);
      continue;
    }
    exports[name] = declaration;
    constPattern.lastIndex = Math.max(constPattern.lastIndex, end);
  }

  return exports;
}

function shouldTrackExportedConstDeclaration(declaration) {
  if (!/\bexport\s+(?:const|let|var)\b/.test(declaration)) {
    return false;
  }

  const [left, right = ""] = declaration.split("=", 2);
  const initializer = right.trim().replace(/;$/, "");

  return (
    declaration.includes("=>") ||
    /\bnew\s+class\b/.test(initializer) ||
    initializer.startsWith("function") ||
    initializer.startsWith("async function") ||
    initializer.startsWith("class") ||
    /\)\s*=>/.test(left)
  );
}

function collectInterfaces(source, masked) {
  const result = {};
  const pattern = /(?:export\s+)?interface\s+([A-Za-z_$][\w$]*)(?:\s+extends\s+([^{}]+))?\s*\{/g;
  let match;
  while ((match = pattern.exec(masked)) !== null) {
    const name = match[1];
    const openIndex = masked.indexOf("{", match.index);
    const closeIndex = findMatching(masked, openIndex, "{", "}");
    if (closeIndex === -1) {
      continue;
    }
    const body = source.slice(openIndex + 1, closeIndex);
    result[name] = {
      exported: match[0].includes("export "),
      members: parseObjectMembers(body),
      extendsRefs: splitTypeReferences(match[2] || ""),
    };
    pattern.lastIndex = closeIndex + 1;
  }
  return result;
}

function collectTypeAliases(source, masked) {
  const result = {};
  const pattern = /(?:export\s+)?type\s+([A-Za-z_$][\w$]*)\s*=/g;
  let match;
  while ((match = pattern.exec(masked)) !== null) {
    const name = match[1];
    const eqIndex = masked.indexOf("=", match.index);
    const end = findStatementEnd(masked, eqIndex + 1);
    const expression = source.slice(eqIndex + 1, end - 1).trim();
    result[name] = {
      exported: match[0].includes("export "),
      ...parseTypeAliasExpression(expression),
    };
    pattern.lastIndex = Math.max(pattern.lastIndex, end);
  }
  return result;
}

function parseTypeAliasExpression(expression) {
  const trimmed = expression.trim();
  if (trimmed.startsWith("{")) {
    const closeIndex = findMatching(trimmed, 0, "{", "}");
    const body = closeIndex === -1 ? trimmed.slice(1) : trimmed.slice(1, closeIndex);
    return {
      members: parseObjectMembers(body),
      refs: [],
    };
  }

  return {
    members: {},
    refs: splitTypeReferences(trimmed),
  };
}

function parseObjectMembers(body) {
  const members = {};
  const lines = body.split("\n");
  for (const line of lines) {
    const trimmed = line.trim().replace(/[;,]$/, "");
    if (!trimmed) {
      continue;
    }

    const method = trimmed.match(/^([A-Za-z_$][\w$]*)\??\s*\((.*)\)\s*:\s*(.+)$/);
    if (method) {
      members[method[1]] = normalizeSignature(`${method[1]}(${method[2]}):${method[3]}`);
      continue;
    }

    const property = trimmed.match(/^([A-Za-z_$][\w$]*)\??\s*:\s*(.+)$/);
    if (property) {
      members[property[1]] = normalizeSignature(`${property[1]}:${property[2]}`);
    }
  }
  return members;
}

function collectClasses(source, masked) {
  const result = {};
  const pattern =
    /(?:export\s+)?class\s+([A-Za-z_$][\w$]*)(?:\s+extends\s+[^{\s]+)?(?:\s+implements\s+([^ {]+(?:\s*,\s*[^ {,]+)*))?[^{]*\{/g;
  let match;
  while ((match = pattern.exec(masked)) !== null) {
    const name = match[1];
    const openIndex = masked.indexOf("{", match.index);
    const closeIndex = findMatching(masked, openIndex, "{", "}");
    if (closeIndex === -1) {
      continue;
    }
    const body = source.slice(openIndex + 1, closeIndex);
    result[name] = {
      exported: match[0].includes("export "),
      implementsRefs: splitTypeReferences(match[2] || ""),
      members: parseClassMembers(body),
      memberKinds: parseMemberKinds(body, name),
    };
    pattern.lastIndex = closeIndex + 1;
  }
  return result;
}

function parseClassMembers(body) {
  const members = {};
  const lines = body.split("\n");
  for (const line of lines) {
    const trimmed = line.trim().replace(/[,{]$/, "").trim();
    if (!trimmed || trimmed.startsWith("constructor(")) {
      continue;
    }

    const arrow = trimmed.match(/^([A-Za-z_$][\w$]*)\s*=\s*(?:async\s*)?\((.*)\)\s*:\s*(.+?)\s*=>/);
    if (arrow) {
      members[arrow[1]] = normalizeSignature(`${arrow[1]}(${arrow[2]}):${arrow[3]}`);
      continue;
    }

    const method = trimmed.match(
      /^(?:public\s+|private\s+|protected\s+|readonly\s+|static\s+|override\s+)*(?:async\s+)?([A-Za-z_$][\w$]*)\s*\((.*)\)\s*:\s*(.+)$/,
    );
    if (method) {
      members[method[1]] = normalizeSignature(`${method[1]}(${method[2]}):${method[3]}`);
      continue;
    }

    const property = trimmed.match(
      /^(?:public\s+|private\s+|protected\s+|readonly\s+|static\s+|override\s+)*([A-Za-z_$][\w$]*)\??\s*:\s*(.+)$/,
    );
    if (property) {
      members[property[1]] = normalizeSignature(`${property[1]}:${property[2]}`);
    }
  }
  return members;
}

function parseMemberKinds(body, className) {
  const kinds = {};
  const lines = body.split("\n");
  for (const line of lines) {
    const trimmed = line.trim();
    if (!trimmed || trimmed.startsWith("constructor(")) {
      continue;
    }

    const getter = trimmed.match(/^get\s+([A-Za-z_$][\w$]*)\s*\(/);
    if (getter) {
      kinds[`${className}.${getter[1]}`] = "getter";
      continue;
    }
    const setter = trimmed.match(/^set\s+([A-Za-z_$][\w$]*)\s*\(/);
    if (setter) {
      kinds[`${className}.${setter[1]}`] = "setter";
      continue;
    }
    const arrow = trimmed.match(/^([A-Za-z_$][\w$]*)\s*=\s*(?:async\s*)?\(/);
    if (arrow) {
      kinds[`${className}.${arrow[1]}`] = "property";
      continue;
    }
    const method = trimmed.match(
      /^(?:public\s+|private\s+|protected\s+|readonly\s+|static\s+|override\s+)*(?:async\s+)?([A-Za-z_$][\w$]*)\s*\(/,
    );
    if (method) {
      kinds[`${className}.${method[1]}`] = "method";
      continue;
    }
    const property = trimmed.match(
      /^(?:public\s+|private\s+|protected\s+|readonly\s+|static\s+|override\s+)*([A-Za-z_$][\w$]*)\??\s*:/,
    );
    if (property) {
      kinds[`${className}.${property[1]}`] = "field";
    }
  }
  return kinds;
}

function resolveShape(reference, modulePath, moduleGraph, seen) {
  const target = resolveReference(reference, modulePath, moduleGraph);
  if (!target) {
    return null;
  }

  const key = `${target.modulePath}::${target.name}`;
  if (seen.has(key)) {
    return { name: target.name, members: {} };
  }
  seen.add(key);

  const moduleInfo = moduleGraph.modules.get(target.modulePath);
  if (!moduleInfo) {
    return null;
  }

  if (moduleInfo.interfaces[target.name]) {
    const iface = moduleInfo.interfaces[target.name];
    const members = { ...iface.members };
    for (const parent of iface.extendsRefs) {
      const parentShape = resolveShape(parent, target.modulePath, moduleGraph, seen);
      if (parentShape) {
        Object.assign(members, parentShape.members);
      }
    }
    return { name: target.name, members };
  }

  if (moduleInfo.typeAliases[target.name]) {
    const alias = moduleInfo.typeAliases[target.name];
    const members = { ...alias.members };
    for (const parent of alias.refs) {
      const parentShape = resolveShape(parent, target.modulePath, moduleGraph, seen);
      if (parentShape) {
        Object.assign(members, parentShape.members);
      }
    }
    return { name: target.name, members };
  }

  return null;
}

function resolveReference(reference, modulePath, moduleGraph) {
  const moduleInfo = moduleGraph.modules.get(modulePath);
  if (!moduleInfo) {
    return null;
  }

  const trimmed = reference.trim();
  if (!trimmed) {
    return null;
  }

  if (trimmed.includes(".")) {
    const [namespace, name] = trimmed.split(".", 2);
    const targetModulePath = moduleInfo.imports.namespace.get(namespace);
    if (!targetModulePath) {
      return null;
    }
    return { modulePath: targetModulePath, name };
  }

  if (moduleInfo.interfaces[trimmed] || moduleInfo.typeAliases[trimmed]) {
    return { modulePath, name: trimmed };
  }

  const namedImport = moduleInfo.imports.named.get(trimmed);
  if (namedImport) {
    return {
      modulePath: namedImport.modulePath,
      name: namedImport.exported,
    };
  }

  return null;
}

function splitTypeReferences(input) {
  return input
    .split(/[,&]/)
    .map((entry) => entry.trim())
    .filter(Boolean)
    .map((entry) => entry.replace(/<.*$/, "").trim());
}

function implementsShape(classMembers, shapeMembers) {
  const names = Object.keys(shapeMembers);
  if (names.length === 0) {
    return false;
  }

  for (const name of names) {
    if (classMembers[name] !== shapeMembers[name]) {
      return false;
    }
  }
  return true;
}

function snapshotFile(moduleInfo) {
  const memberKinds = {};
  for (const snapshot of Object.values(moduleInfo.classes)) {
    Object.assign(memberKinds, snapshot.memberKinds);
  }

  return {
    path: moduleInfo.path,
    async_functions: count(moduleInfo.masked, /\basync\b/g),
    await_expressions: count(moduleInfo.masked, /\bawait\b/g),
    promise_calls: count(moduleInfo.masked, /\bPromise(?:\.[A-Za-z_$][\w$]*)?\b/g),
    timers: count(moduleInfo.masked, /\b(?:setTimeout|setInterval|queueMicrotask)\b/g),
    max_nesting: approximateBranchNesting(moduleInfo.source),
    try_blocks: count(moduleInfo.masked, /\btry\b/g),
    catch_clauses: count(moduleInfo.masked, /\bcatch\b/g),
    throw_statements: count(moduleInfo.masked, /\bthrow\b/g),
    instanceof_error_checks: count(moduleInfo.masked, /\binstanceof\s+Error\b/g),
    date_calls: count(moduleInfo.masked, /\bDate\.now\b|\bnew\s+Date\b/g),
    retry_markers: count(moduleInfo.masked, /\b(?:retry|retries|backoff|attempt)\b/gi),
    member_kinds: memberKinds,
    abort_controllers: count(moduleInfo.masked, /\bnew\s+AbortController\b/g),
    cleanup_calls: count(
      moduleInfo.masked,
      /\b(?:clearTimeout|clearInterval|removeEventListener|unsubscribe|dispose|disconnect|abort)\b/g,
    ),
  };
}

function approximateBranchNesting(source) {
  const lines = source.split("\n");
  let currentDepth = 0;
  let maxDepth = 0;

  for (const line of lines) {
    const trimmed = line.trim();
    const closing = (trimmed.match(/\}/g) || []).length;
    currentDepth = Math.max(0, currentDepth - closing);

    if (/^(if|for|while|switch|try|catch)\b/.test(trimmed)) {
      maxDepth = Math.max(maxDepth, currentDepth + 1);
    }

    const opening = (trimmed.match(/\{/g) || []).length;
    currentDepth += opening;
  }

  return maxDepth;
}

function maskSource(source) {
  let output = "";
  let mode = "code";
  let quote = "";

  for (let index = 0; index < source.length; index += 1) {
    const ch = source[index];
    const next = source[index + 1];

    if (mode === "line_comment") {
      output += ch === "\n" ? "\n" : " ";
      if (ch === "\n") {
        mode = "code";
      }
      continue;
    }

    if (mode === "block_comment") {
      output += ch === "\n" ? "\n" : " ";
      if (ch === "*" && next === "/") {
        output += " ";
        index += 1;
        mode = "code";
      }
      continue;
    }

    if (mode === "string") {
      output += ch === "\n" ? "\n" : " ";
      if (ch === "\\") {
        output += " ";
        index += 1;
        continue;
      }
      if (ch === quote) {
        mode = "code";
      }
      continue;
    }

    if (ch === "/" && next === "/") {
      output += "  ";
      index += 1;
      mode = "line_comment";
      continue;
    }
    if (ch === "/" && next === "*") {
      output += "  ";
      index += 1;
      mode = "block_comment";
      continue;
    }
    if (ch === "'" || ch === "\"" || ch === "`") {
      output += " ";
      mode = "string";
      quote = ch;
      continue;
    }

    output += ch;
  }

  return output;
}

function findMatching(source, openIndex, openChar, closeChar) {
  let depth = 0;
  for (let index = openIndex; index < source.length; index += 1) {
    const ch = source[index];
    if (ch === openChar) {
      depth += 1;
    } else if (ch === closeChar) {
      depth -= 1;
      if (depth === 0) {
        return index;
      }
    }
  }
  return -1;
}

function findStatementEnd(source, start) {
  let braceDepth = 0;
  let parenDepth = 0;
  let angleDepth = 0;

  for (let index = start; index < source.length; index += 1) {
    const ch = source[index];
    if (ch === "{") {
      braceDepth += 1;
      continue;
    }
    if (ch === "}") {
      braceDepth = Math.max(0, braceDepth - 1);
      continue;
    }
    if (ch === "(") {
      parenDepth += 1;
      continue;
    }
    if (ch === ")") {
      parenDepth = Math.max(0, parenDepth - 1);
      continue;
    }
    if (ch === "<") {
      angleDepth += 1;
      continue;
    }
    if (ch === ">") {
      angleDepth = Math.max(0, angleDepth - 1);
      continue;
    }
    if (ch === ";" && braceDepth === 0 && parenDepth === 0 && angleDepth === 0) {
      return index + 1;
    }
  }

  return source.length;
}

function normalizeSignature(value) {
  return value.replace(/\s+/g, " ").trim();
}

function count(source, pattern) {
  const matches = source.match(pattern);
  return matches ? matches.length : 0;
}

function normalizePath(value) {
  return value.split(path.sep).join("/");
}

function normalizeDir(value) {
  return value === "." || value === "" ? "." : value;
}

function isTypeScriptFile(file) {
  return /\.(ts|tsx)$/.test(file);
}

function isTestFile(file) {
  return /(?:\.test|\.spec)\.(?:ts|tsx)$/.test(file) || /\/tests?\//.test(file);
}

main();
