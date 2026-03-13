use std::{
    collections::{HashMap, HashSet},
    path::Path,
};

use serde::{Deserialize, Serialize};

use crate::{
    AnalysisContext, AnalyzerPlugin, Confidence, InputKind, PluginAnalysis, PluginFinding,
    ReasonKind, normalized_test_oracle_lines,
    plugins::{
        helper_process::{EmbeddedHelper, run_embedded_json_helper},
        support::{additive_finding, base_finding, weighted_base_finding},
    },
};

const HELPER_MAIN_JS: &str = include_str!("../../tools/typescript-analyzer/main.js");
const TYPESCRIPT_HELPER: EmbeddedHelper<'static> = EmbeddedHelper {
    temp_dir_prefix: "typescript-helper",
    files: &[("main.js", HELPER_MAIN_JS)],
    program: "node",
    args: &["main.js"],
};

pub struct TypeScriptPlugin;

impl TypeScriptPlugin {
    pub fn new() -> Self {
        Self
    }
}

impl AnalyzerPlugin for TypeScriptPlugin {
    fn id(&self) -> &'static str {
        "ts"
    }

    fn analyze(&self, ctx: &AnalysisContext) -> PluginAnalysis {
        let ts_files = changed_typescript_files(ctx);
        if ts_files.is_empty() {
            return PluginAnalysis::new(Confidence::High, Vec::new());
        }

        let helper_inputs = match resolve_revision_inputs(ctx, &ts_files) {
            Ok(inputs) => inputs,
            Err(fallback) => return fallback,
        };

        let before = match run_helper(&helper_inputs.before_workspace, &ts_files) {
            Ok(snapshot) => snapshot,
            Err(error) => {
                return fallback_findings(ctx, &format!("typescript helper failed: {error}"));
            }
        };
        let after = match run_helper(&helper_inputs.after_workspace, &ts_files) {
            Ok(snapshot) => snapshot,
            Err(error) => {
                return fallback_findings(ctx, &format!("typescript helper failed: {error}"));
            }
        };

        let mut findings = Vec::new();
        let mut by_dir = HashMap::<String, String>::new();
        for path in &ts_files {
            let mut dir = Path::new(path)
                .parent()
                .unwrap_or_else(|| Path::new("."))
                .to_string_lossy()
                .to_string();
            if dir.is_empty() {
                dir = String::from(".");
            }
            by_dir.entry(dir).or_insert_with(|| path.clone());
        }

        for (dir, path) in &by_dir {
            let before_exports = before
                .packages
                .get(dir)
                .map(|snapshot| &snapshot.exports)
                .cloned()
                .unwrap_or_default();
            let after_exports = after
                .packages
                .get(dir)
                .map(|snapshot| &snapshot.exports)
                .cloned()
                .unwrap_or_default();
            if before_exports != after_exports {
                findings.push(base_finding(
                    path.clone(),
                    ReasonKind::TypeScriptExportedApiChange,
                    "exported typescript api changed",
                ));
            }

            let before_impls: HashSet<_> = before
                .packages
                .get(dir)
                .map(|snapshot| snapshot.implementations.clone())
                .unwrap_or_default()
                .into_iter()
                .collect();
            let after_impls: HashSet<_> = after
                .packages
                .get(dir)
                .map(|snapshot| snapshot.implementations.clone())
                .unwrap_or_default()
                .into_iter()
                .collect();
            let removed: Vec<_> = before_impls.difference(&after_impls).cloned().collect();
            if !removed.is_empty() {
                findings.push(base_finding(
                    path.clone(),
                    ReasonKind::TypeScriptInterfaceBreak,
                    format!(
                        "typescript interface implementation removed: {}",
                        removed.join(", ")
                    ),
                ));
            }
        }

        for path in &ts_files {
            let before_file = before.files.get(path);
            let after_file = after.files.get(path);

            if member_kind_changed(before_file, after_file) {
                findings.push(base_finding(
                    path.clone(),
                    ReasonKind::TypeScriptMemberKindChange,
                    "typescript member kind changed",
                ));
            }
            if async_changed(before_file, after_file) {
                findings.push(weighted_base_finding(
                    path.clone(),
                    ReasonKind::TypeScriptAsyncChange,
                    format!(
                        "typescript async change (nesting {})",
                        async_nesting(after_file)
                    ),
                    async_weight(after_file),
                ));
            }
            if error_handling_changed(before_file, after_file) {
                findings.push(base_finding(
                    path.clone(),
                    ReasonKind::TypeScriptErrorHandlingChange,
                    "typescript error handling changed",
                ));
            }
            if runtime_behavior_changed(before_file, after_file) {
                findings.push(base_finding(
                    path.clone(),
                    ReasonKind::TypeScriptRuntimeBehaviorChange,
                    "typescript runtime behavior changed",
                ));
            }
            if resource_lifecycle_changed(before_file, after_file) {
                findings.push(base_finding(
                    path.clone(),
                    ReasonKind::TypeScriptResourceLifecycleChange,
                    "typescript resource lifecycle changed",
                ));
            }
        }

        for file in &ctx.files {
            if !is_typescript_test_file(&file.path) {
                continue;
            }
            if typescript_test_oracle_changed(file) {
                findings.push(base_finding(
                    file.path.clone(),
                    ReasonKind::TypeScriptTestOracleChange,
                    "typescript test oracle changed",
                ));
            }
        }

        PluginAnalysis::new(Confidence::High, findings)
    }
}

struct RevisionInputs {
    before_workspace: std::path::PathBuf,
    after_workspace: std::path::PathBuf,
}

fn changed_typescript_files(ctx: &AnalysisContext) -> Vec<String> {
    ctx.files
        .iter()
        .filter(|file| is_typescript_path(&file.path))
        .map(|file| file.path.clone())
        .collect()
}

fn resolve_revision_inputs(
    ctx: &AnalysisContext,
    ts_files: &[String],
) -> Result<RevisionInputs, PluginAnalysis> {
    if ctx.input_kind != InputKind::GitRevisionRange {
        return Err(fallback_findings(
            ctx,
            "typescript plugin requires git revision input",
        ));
    }

    let Some(before_workspace) = &ctx.before_workspace else {
        return Err(fallback_findings(
            ctx,
            "typescript plugin requires before workspace",
        ));
    };
    let Some(after_workspace) = &ctx.after_workspace else {
        return Err(fallback_findings(
            ctx,
            "typescript plugin requires after workspace",
        ));
    };

    if !before_workspace.join("package.json").exists()
        || !after_workspace.join("package.json").exists()
    {
        return Err(fallback_findings(
            ctx,
            "typescript plugin requires package.json",
        ));
    }

    if ts_files.is_empty() {
        return Err(PluginAnalysis::new(Confidence::High, Vec::new()));
    }

    Ok(RevisionInputs {
        before_workspace: before_workspace.clone(),
        after_workspace: after_workspace.clone(),
    })
}

fn fallback_findings(ctx: &AnalysisContext, reason: &str) -> PluginAnalysis {
    let mut findings = Vec::new();

    for file in &ctx.files {
        if !is_typescript_path(&file.path) {
            continue;
        }

        findings.push(additive_finding(
            file.path.clone(),
            ReasonKind::TypeScriptAnalysisFallback,
            reason.to_string(),
        ));
        fallback_enrich(file, &mut findings);
    }

    PluginAnalysis::new(Confidence::Medium, findings)
}

#[derive(Serialize)]
struct HelperRequest {
    workspace_root: String,
    changed_files: Vec<String>,
}

#[derive(Deserialize)]
struct HelperResponse {
    packages: Vec<HelperPackageSnapshot>,
    files: Vec<HelperFileSnapshot>,
}

#[derive(Clone, Deserialize)]
struct HelperPackageSnapshot {
    dir: String,
    exports: HashMap<String, String>,
    implementations: Vec<String>,
}

#[derive(Clone, Deserialize)]
struct HelperFileSnapshot {
    path: String,
    async_functions: u32,
    await_expressions: u32,
    promise_calls: u32,
    timers: u32,
    max_nesting: u32,
    try_blocks: u32,
    catch_clauses: u32,
    throw_statements: u32,
    instanceof_error_checks: u32,
    date_calls: u32,
    retry_markers: u32,
    member_kinds: HashMap<String, String>,
    abort_controllers: u32,
    cleanup_calls: u32,
}

struct Snapshot {
    packages: HashMap<String, HelperPackageSnapshot>,
    files: HashMap<String, HelperFileSnapshot>,
}

fn run_helper(workspace_root: &Path, changed_files: &[String]) -> Result<Snapshot, String> {
    let request = HelperRequest {
        workspace_root: workspace_root.to_string_lossy().to_string(),
        changed_files: changed_files.to_vec(),
    };
    let response = run_embedded_json_helper::<_, HelperResponse>(&TYPESCRIPT_HELPER, &request)?;

    Ok(Snapshot {
        packages: response
            .packages
            .into_iter()
            .map(|snapshot| (snapshot.dir.clone(), snapshot))
            .collect(),
        files: response
            .files
            .into_iter()
            .map(|snapshot| (snapshot.path.clone(), snapshot))
            .collect(),
    })
}

fn fallback_enrich(file: &crate::ChangedFile, findings: &mut Vec<PluginFinding>) {
    if file
        .added
        .iter()
        .chain(file.removed.iter())
        .any(|line| is_async_signal(line))
    {
        findings.push(weighted_base_finding(
            file.path.clone(),
            ReasonKind::TypeScriptAsyncChange,
            "typescript async change",
            fallback_async_weight(file),
        ));
    }

    if file
        .added
        .iter()
        .chain(file.removed.iter())
        .any(|line| is_exported_typescript_declaration(line))
    {
        findings.push(base_finding(
            file.path.clone(),
            ReasonKind::TypeScriptExportedApiChange,
            "exported typescript api change",
        ));
    }
}

fn async_changed(before: Option<&HelperFileSnapshot>, after: Option<&HelperFileSnapshot>) -> bool {
    let before = snapshot_or_default(before);
    let after = snapshot_or_default(after);

    let before_has_async = has_async_primitives(&before);
    let after_has_async = has_async_primitives(&after);

    before.async_functions != after.async_functions
        || before.await_expressions != after.await_expressions
        || before.promise_calls != after.promise_calls
        || before.timers != after.timers
        || ((before_has_async || after_has_async) && before.max_nesting != after.max_nesting)
}

fn member_kind_changed(
    before: Option<&HelperFileSnapshot>,
    after: Option<&HelperFileSnapshot>,
) -> bool {
    let before = before.map(|value| &value.member_kinds);
    let after = after.map(|value| &value.member_kinds);
    before != after
}

fn async_nesting(snapshot: Option<&HelperFileSnapshot>) -> u32 {
    snapshot.map(|value| value.max_nesting).unwrap_or_default()
}

fn has_async_primitives(snapshot: &HelperFileSnapshot) -> bool {
    snapshot.async_functions > 0
        || snapshot.await_expressions > 0
        || snapshot.promise_calls > 0
        || snapshot.timers > 0
}

fn error_handling_changed(
    before: Option<&HelperFileSnapshot>,
    after: Option<&HelperFileSnapshot>,
) -> bool {
    let before = snapshot_or_default(before);
    let after = snapshot_or_default(after);

    before.try_blocks != after.try_blocks
        || before.catch_clauses != after.catch_clauses
        || before.throw_statements != after.throw_statements
        || before.instanceof_error_checks != after.instanceof_error_checks
}

fn runtime_behavior_changed(
    before: Option<&HelperFileSnapshot>,
    after: Option<&HelperFileSnapshot>,
) -> bool {
    let before = snapshot_or_default(before);
    let after = snapshot_or_default(after);

    before.date_calls != after.date_calls || before.retry_markers != after.retry_markers
}

fn resource_lifecycle_changed(
    before: Option<&HelperFileSnapshot>,
    after: Option<&HelperFileSnapshot>,
) -> bool {
    let before = snapshot_or_default(before);
    let after = snapshot_or_default(after);

    before.abort_controllers != after.abort_controllers
        || before.cleanup_calls != after.cleanup_calls
}

fn snapshot_or_default(snapshot: Option<&HelperFileSnapshot>) -> HelperFileSnapshot {
    snapshot.cloned().unwrap_or_default()
}

impl Default for HelperFileSnapshot {
    fn default() -> Self {
        Self {
            path: String::new(),
            async_functions: 0,
            await_expressions: 0,
            promise_calls: 0,
            timers: 0,
            max_nesting: 0,
            try_blocks: 0,
            catch_clauses: 0,
            throw_statements: 0,
            instanceof_error_checks: 0,
            date_calls: 0,
            retry_markers: 0,
            member_kinds: HashMap::new(),
            abort_controllers: 0,
            cleanup_calls: 0,
        }
    }
}

fn typescript_test_oracle_changed(file: &crate::ChangedFile) -> bool {
    if !is_typescript_test_file(&file.path) {
        return false;
    }

    let removed = normalized_test_oracle_lines(&file.removed);
    let added = normalized_test_oracle_lines(&file.added);

    if removed.is_empty() && added.is_empty() {
        return false;
    }

    removed != added
}

fn async_weight(snapshot: Option<&HelperFileSnapshot>) -> u32 {
    match async_nesting(snapshot) {
        depth if depth >= 4 => 35,
        3 => 30,
        2 => 25,
        _ => 20,
    }
}

fn fallback_async_weight(file: &crate::ChangedFile) -> u32 {
    match approximate_branch_nesting(file) {
        depth if depth >= 4 => 35,
        3 => 30,
        2 => 25,
        _ => 20,
    }
}

fn approximate_branch_nesting(file: &crate::ChangedFile) -> usize {
    let mut current_depth = 0usize;
    let mut max_depth = 0usize;

    for line in &file.added {
        let trimmed = line.trim();

        let closing_braces = trimmed.chars().filter(|ch| *ch == '}').count();
        current_depth = current_depth.saturating_sub(closing_braces);

        if starts_branch(trimmed) {
            max_depth = max_depth.max(current_depth + 1);
        }

        let opening_braces = trimmed.chars().filter(|ch| *ch == '{').count();
        current_depth += opening_braces;
    }

    max_depth
}

fn starts_branch(trimmed: &str) -> bool {
    ["if ", "if(", "for ", "while ", "switch ", "try", "catch"]
        .iter()
        .any(|keyword| trimmed.starts_with(keyword))
}

fn is_exported_typescript_declaration(line: &str) -> bool {
    let trimmed = line.trim_start();
    [
        "export function ",
        "export async function ",
        "export class ",
        "export interface ",
        "export type ",
        "export const ",
    ]
    .iter()
    .any(|prefix| trimmed.starts_with(prefix))
}

fn is_async_signal(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed.contains("await ")
        || trimmed.contains("Promise.")
        || trimmed.contains("Promise<")
        || trimmed.contains("async ")
        || trimmed.contains("setTimeout(")
        || trimmed.contains("queueMicrotask(")
}

fn is_typescript_path(path: &str) -> bool {
    path.ends_with(".ts") || path.ends_with(".tsx")
}

fn is_typescript_test_file(path: &str) -> bool {
    path.ends_with(".test.ts")
        || path.ends_with(".spec.ts")
        || path.ends_with(".test.tsx")
        || path.ends_with(".spec.tsx")
}
