use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::{
    AnalysisContext, AnalyzerPlugin, PluginAnalysis, PluginFinding, ReasonKind,
    normalized_test_oracle_lines,
    plugins::{
        helper_process::{
            EmbeddedHelper, RevisionHelperFallback, resolve_revision_helper_inputs_matching,
            run_embedded_json_helper,
        },
        runtime::{PackageSnapshotView, RevisionSnapshotView, analyze_revision_plugin},
        support::{base_finding, weighted_base_finding},
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
        let helper_inputs = match resolve_revision_helper_inputs_matching(
            ctx,
            is_typescript_path,
            &["package.json"],
            RevisionHelperFallback {
                kind: ReasonKind::TypeScriptAnalysisFallback,
                input_kind_reason: "typescript plugin requires git revision input",
                repo_root_reason: "typescript plugin requires repo root",
                base_rev_reason: "typescript plugin requires base revision",
                head_rev_reason: "typescript plugin requires head revision",
                required_files_reason: "typescript plugin requires package.json",
            },
            fallback_enrich,
        ) {
            Ok(inputs) => inputs,
            Err(fallback) => return fallback,
        };
        analyze_revision_plugin(
            ctx,
            helper_inputs,
            run_helper,
            |ctx, error| fallback_findings(ctx, &format!("typescript helper failed: {error}")),
            analyze_package_findings,
            analyze_file_findings,
            analyze_test_findings,
        )
    }
}

fn fallback_findings(ctx: &AnalysisContext, reason: &str) -> PluginAnalysis {
    crate::plugins::support::fallback_analysis_matching(
        ctx,
        is_typescript_path,
        ReasonKind::TypeScriptAnalysisFallback,
        reason,
        fallback_enrich,
    )
}

#[derive(Serialize)]
struct HelperRequest {
    repo_root: String,
    base_rev: String,
    head_rev: String,
    changed_files: Vec<String>,
}

#[derive(Deserialize)]
struct HelperResponse {
    before: HelperRevisionSnapshot,
    after: HelperRevisionSnapshot,
}

#[derive(Deserialize)]
struct HelperRevisionSnapshot {
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

impl PackageSnapshotView for HelperPackageSnapshot {
    fn exports(&self) -> &HashMap<String, String> {
        &self.exports
    }

    fn implementations(&self) -> &[String] {
        &self.implementations
    }
}

impl RevisionSnapshotView for Snapshot {
    type Package = HelperPackageSnapshot;
    type File = HelperFileSnapshot;

    fn package_snapshot(&self, dir: &str) -> Option<&Self::Package> {
        self.packages.get(dir)
    }

    fn file_snapshot(&self, path: &str) -> Option<&Self::File> {
        self.files.get(path)
    }
}

fn run_helper(inputs: &crate::plugins::helper_process::RevisionHelperInputs) -> Result<(Snapshot, Snapshot), String> {
    let request = HelperRequest {
        repo_root: inputs.repo_root.to_string_lossy().to_string(),
        base_rev: inputs.base_rev.clone(),
        head_rev: inputs.head_rev.clone(),
        changed_files: inputs.changed_files.clone(),
    };
    let response = run_embedded_json_helper::<_, HelperResponse>(&TYPESCRIPT_HELPER, &request)?;

    Ok((
        Snapshot {
            packages: response
                .before
                .packages
                .into_iter()
                .map(|snapshot| (snapshot.dir.clone(), snapshot))
                .collect(),
            files: response
                .before
                .files
                .into_iter()
                .map(|snapshot| (snapshot.path.clone(), snapshot))
                .collect(),
        },
        Snapshot {
            packages: response
                .after
                .packages
                .into_iter()
                .map(|snapshot| (snapshot.dir.clone(), snapshot))
                .collect(),
            files: response
                .after
                .files
                .into_iter()
                .map(|snapshot| (snapshot.path.clone(), snapshot))
                .collect(),
        },
    ))
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

    if !is_internal_or_private_path(&file.path)
        && file
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

fn analyze_package_findings(
    path: &str,
    before: Option<&HelperPackageSnapshot>,
    after: Option<&HelperPackageSnapshot>,
    findings: &mut Vec<PluginFinding>,
) {
    if is_internal_or_private_path(path) {
        return;
    }

    let before_exports = before
        .map(|snapshot| snapshot.exports.clone())
        .unwrap_or_default();
    let after_exports = after
        .map(|snapshot| snapshot.exports.clone())
        .unwrap_or_default();
    if before_exports != after_exports {
        findings.push(base_finding(
            path.to_string(),
            ReasonKind::TypeScriptExportedApiChange,
            "exported typescript api changed",
        ));
    }

    let before_impls: HashSet<_> = before
        .map(|snapshot| snapshot.implementations.clone())
        .unwrap_or_default()
        .into_iter()
        .collect();
    let after_impls: HashSet<_> = after
        .map(|snapshot| snapshot.implementations.clone())
        .unwrap_or_default()
        .into_iter()
        .collect();
    let removed: Vec<_> = before_impls.difference(&after_impls).cloned().collect();
    if !removed.is_empty() {
        findings.push(base_finding(
            path.to_string(),
            ReasonKind::TypeScriptInterfaceBreak,
            format!(
                "typescript interface implementation removed: {}",
                removed.join(", ")
            ),
        ));
    }
}

fn analyze_file_findings(
    path: &str,
    before: Option<&HelperFileSnapshot>,
    after: Option<&HelperFileSnapshot>,
    findings: &mut Vec<PluginFinding>,
) {
    if member_kind_changed(before, after) {
        findings.push(base_finding(
            path.to_string(),
            ReasonKind::TypeScriptMemberKindChange,
            "typescript member kind changed",
        ));
    }
    if async_changed(before, after) {
        findings.push(weighted_base_finding(
            path.to_string(),
            ReasonKind::TypeScriptAsyncChange,
            format!("typescript async change (nesting {})", async_nesting(after)),
            async_weight(after),
        ));
    }
    if error_handling_changed(before, after) {
        findings.push(base_finding(
            path.to_string(),
            ReasonKind::TypeScriptErrorHandlingChange,
            "typescript error handling changed",
        ));
    }
    if runtime_behavior_changed(before, after) {
        findings.push(base_finding(
            path.to_string(),
            ReasonKind::TypeScriptRuntimeBehaviorChange,
            "typescript runtime behavior changed",
        ));
    }
    if resource_lifecycle_changed(before, after) {
        findings.push(base_finding(
            path.to_string(),
            ReasonKind::TypeScriptResourceLifecycleChange,
            "typescript resource lifecycle changed",
        ));
    }
}

fn analyze_test_findings(file: &crate::ChangedFile, findings: &mut Vec<PluginFinding>) {
    if is_typescript_test_file(&file.path) && typescript_test_oracle_changed(file) {
        findings.push(base_finding(
            file.path.clone(),
            ReasonKind::TypeScriptTestOracleChange,
            "typescript test oracle changed",
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
    ]
    .iter()
    .any(|prefix| trimmed.starts_with(prefix))
        || is_function_like_exported_const(trimmed)
}

fn is_function_like_exported_const(line: &str) -> bool {
    let Some(rest) = line
        .strip_prefix("export const ")
        .or_else(|| line.strip_prefix("export let "))
        .or_else(|| line.strip_prefix("export var "))
    else {
        return false;
    };

    rest.contains("=>")
        || rest
            .split_once('=')
            .map(|(_, value)| {
                let value = value.trim();
                value.starts_with("function")
                    || value.starts_with("async function")
                    || value.starts_with("class")
            })
            .unwrap_or(false)
}

fn is_internal_or_private_path(path: &str) -> bool {
    path.split('/').any(|segment| {
        segment == "internal"
            || segment == "(private)"
            || (segment.starts_with('_') && segment.len() > 1)
    })
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
