use std::{
    collections::{HashMap, HashSet},
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};

use crate::{
    AnalysisContext, AnalyzerPlugin, Confidence, InputKind, PluginAnalysis, PluginFinding,
    PluginScoreMode, ReasonKind,
};

const HELPER_GO_MOD: &str = include_str!("../../tools/go-analyzer/go.mod");
const HELPER_GO_SUM: &str = include_str!("../../tools/go-analyzer/go.sum");
const HELPER_MAIN_GO: &str = include_str!("../../tools/go-analyzer/main.go");
static HELPER_TEMP_DIR_COUNTER: AtomicU64 = AtomicU64::new(0);

pub struct GoPlugin;

impl GoPlugin {
    pub fn new() -> Self {
        Self
    }
}

impl AnalyzerPlugin for GoPlugin {
    fn id(&self) -> &'static str {
        "go"
    }

    fn analyze(&self, ctx: &AnalysisContext) -> PluginAnalysis {
        let go_files = changed_go_files(ctx);
        if go_files.is_empty() {
            return PluginAnalysis::new(Confidence::High, Vec::new());
        }

        if ctx.input_kind != InputKind::GitRevisionRange {
            return fallback_findings(ctx, "go plugin requires git revision input");
        }

        let Some(before_workspace) = &ctx.before_workspace else {
            return fallback_findings(ctx, "go plugin requires before workspace");
        };
        let Some(after_workspace) = &ctx.after_workspace else {
            return fallback_findings(ctx, "go plugin requires after workspace");
        };

        if !before_workspace.join("go.mod").exists() || !after_workspace.join("go.mod").exists() {
            return fallback_findings(ctx, "go plugin requires go.mod");
        }

        let before = match run_helper(before_workspace, &go_files) {
            Ok(snapshot) => snapshot,
            Err(error) => return fallback_findings(ctx, &format!("go helper failed: {error}")),
        };
        let after = match run_helper(after_workspace, &go_files) {
            Ok(snapshot) => snapshot,
            Err(error) => return fallback_findings(ctx, &format!("go helper failed: {error}")),
        };

        let mut findings = Vec::new();
        let mut by_dir = HashMap::<String, String>::new();
        for path in &go_files {
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
                findings.push(PluginFinding {
                    path: path.clone(),
                    kind: ReasonKind::GoExportedApiChange,
                    message: String::from("go exported api changed"),
                    weight_override: None,
                    score_mode: PluginScoreMode::Base,
                });
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
                findings.push(PluginFinding {
                    path: path.clone(),
                    kind: ReasonKind::GoInterfaceBreak,
                    message: format!(
                        "go interface implementation removed: {}",
                        removed.join(", ")
                    ),
                    weight_override: None,
                    score_mode: PluginScoreMode::Base,
                });
            }
        }

        for path in &go_files {
            let before_file = before.files.get(path);
            let after_file = after.files.get(path);
            if receiver_changed(before_file, after_file) {
                findings.push(PluginFinding {
                    path: path.clone(),
                    kind: ReasonKind::GoReceiverChange,
                    message: String::from("go receiver kind changed"),
                    weight_override: None,
                    score_mode: PluginScoreMode::Base,
                });
            }
            if concurrency_changed(before_file, after_file) {
                findings.push(PluginFinding {
                    path: path.clone(),
                    kind: ReasonKind::GoConcurrencyChange,
                    message: format!(
                        "go concurrency primitive changed (nesting {})",
                        concurrency_nesting(after_file)
                    ),
                    weight_override: Some(go_concurrency_weight(after_file)),
                    score_mode: PluginScoreMode::Base,
                });
            }

            if error_handling_changed(before_file, after_file) {
                findings.push(PluginFinding {
                    path: path.clone(),
                    kind: ReasonKind::GoErrorHandlingChange,
                    message: String::from("go error handling changed"),
                    weight_override: None,
                    score_mode: PluginScoreMode::Base,
                });
            }

            if runtime_behavior_changed(before_file, after_file) {
                findings.push(PluginFinding {
                    path: path.clone(),
                    kind: ReasonKind::GoRuntimeBehaviorChange,
                    message: String::from("go runtime behavior changed"),
                    weight_override: None,
                    score_mode: PluginScoreMode::Base,
                });
            }
            if resource_lifecycle_changed(before_file, after_file) {
                findings.push(PluginFinding {
                    path: path.clone(),
                    kind: ReasonKind::GoResourceLifecycleChange,
                    message: String::from("go resource lifecycle changed"),
                    weight_override: None,
                    score_mode: PluginScoreMode::Base,
                });
            }
        }

        for file in &ctx.files {
            if !file.path.ends_with("_test.go") {
                continue;
            }
            if go_test_oracle_changed(file) {
                findings.push(PluginFinding {
                    path: file.path.clone(),
                    kind: ReasonKind::GoTestOracleChange,
                    message: String::from("go test oracle changed"),
                    weight_override: None,
                    score_mode: PluginScoreMode::Base,
                });
            }
        }

        PluginAnalysis::new(Confidence::High, findings)
    }
}

fn changed_go_files(ctx: &AnalysisContext) -> Vec<String> {
    ctx.files
        .iter()
        .filter(|file| file.path.ends_with(".go"))
        .map(|file| file.path.clone())
        .collect()
}

fn fallback_findings(ctx: &AnalysisContext, reason: &str) -> PluginAnalysis {
    let mut findings = Vec::new();

    for file in &ctx.files {
        if !file.path.ends_with(".go") {
            continue;
        }

        findings.push(PluginFinding {
            path: file.path.clone(),
            kind: ReasonKind::GoAnalysisFallback,
            message: reason.to_string(),
            weight_override: None,
            score_mode: PluginScoreMode::Additive,
        });

        if file
            .added
            .iter()
            .chain(file.removed.iter())
            .any(|line| line.trim_start().starts_with("select {"))
        {
            findings.push(PluginFinding {
                path: file.path.clone(),
                kind: ReasonKind::GoConcurrencyChange,
                message: String::from("go select change"),
                weight_override: Some(fallback_concurrency_weight(file)),
                score_mode: PluginScoreMode::Base,
            });
        }

        if file
            .added
            .iter()
            .chain(file.removed.iter())
            .any(|line| is_exported_go_declaration(line))
        {
            findings.push(PluginFinding {
                path: file.path.clone(),
                kind: ReasonKind::GoExportedApiChange,
                message: String::from("exported go api change"),
                weight_override: None,
                score_mode: PluginScoreMode::Base,
            });
        }
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
    goroutines: u32,
    defers: u32,
    selects: u32,
    sends: u32,
    receives: u32,
    closes: u32,
    max_nesting: u32,
    errors_is_as_calls: u32,
    nil_checks: u32,
    panic_calls: u32,
    recover_calls: u32,
    context_checks: u32,
    time_calls: u32,
    retry_markers: u32,
    receiver_kinds: HashMap<String, String>,
    cleanup_calls: u32,
}

struct Snapshot {
    packages: HashMap<String, HelperPackageSnapshot>,
    files: HashMap<String, HelperFileSnapshot>,
}

fn run_helper(workspace_root: &Path, changed_files: &[String]) -> Result<Snapshot, String> {
    let helper_dir = unique_temp_dir("go-helper");
    fs::write(helper_dir.join("go.mod"), HELPER_GO_MOD).map_err(|error| error.to_string())?;
    fs::write(helper_dir.join("go.sum"), HELPER_GO_SUM).map_err(|error| error.to_string())?;
    fs::write(helper_dir.join("main.go"), HELPER_MAIN_GO).map_err(|error| error.to_string())?;

    let request = HelperRequest {
        workspace_root: workspace_root.to_string_lossy().to_string(),
        changed_files: changed_files.to_vec(),
    };
    let request_json = serde_json::to_vec(&request).map_err(|error| error.to_string())?;

    let mut child = Command::new("go")
        .arg("run")
        .arg(".")
        .current_dir(&helper_dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| error.to_string())?;

    if let Some(stdin) = child.stdin.as_mut() {
        stdin
            .write_all(&request_json)
            .map_err(|error| error.to_string())?;
    }

    let output = child
        .wait_with_output()
        .map_err(|error| error.to_string())?;
    let _ = fs::remove_dir_all(&helper_dir);

    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }

    let response: HelperResponse =
        serde_json::from_slice(&output.stdout).map_err(|error| error.to_string())?;

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

fn unique_temp_dir(prefix: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("time should move forward")
        .as_nanos();
    let counter = HELPER_TEMP_DIR_COUNTER.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "shiwake-{prefix}-{}-{nanos}-{counter}",
        std::process::id()
    ));
    fs::create_dir_all(&path).expect("temp helper dir should be created");
    path
}

fn concurrency_changed(
    before: Option<&HelperFileSnapshot>,
    after: Option<&HelperFileSnapshot>,
) -> bool {
    let before = before.cloned().unwrap_or(HelperFileSnapshot {
        path: String::new(),
        goroutines: 0,
        defers: 0,
        selects: 0,
        sends: 0,
        receives: 0,
        closes: 0,
        max_nesting: 0,
        errors_is_as_calls: 0,
        nil_checks: 0,
        panic_calls: 0,
        recover_calls: 0,
        context_checks: 0,
        time_calls: 0,
        retry_markers: 0,
        receiver_kinds: HashMap::new(),
        cleanup_calls: 0,
    });
    let after = after.cloned().unwrap_or(HelperFileSnapshot {
        path: String::new(),
        goroutines: 0,
        defers: 0,
        selects: 0,
        sends: 0,
        receives: 0,
        closes: 0,
        max_nesting: 0,
        errors_is_as_calls: 0,
        nil_checks: 0,
        panic_calls: 0,
        recover_calls: 0,
        context_checks: 0,
        time_calls: 0,
        retry_markers: 0,
        receiver_kinds: HashMap::new(),
        cleanup_calls: 0,
    });

    let before_has_concurrency = has_concurrency_primitives(&before);
    let after_has_concurrency = has_concurrency_primitives(&after);

    before.goroutines != after.goroutines
        || before.defers != after.defers
        || before.selects != after.selects
        || before.sends != after.sends
        || before.receives != after.receives
        || before.closes != after.closes
        || ((before_has_concurrency || after_has_concurrency)
            && before.max_nesting != after.max_nesting)
}

fn receiver_changed(before: Option<&HelperFileSnapshot>, after: Option<&HelperFileSnapshot>) -> bool {
    let before = before.map(|value| &value.receiver_kinds);
    let after = after.map(|value| &value.receiver_kinds);
    before != after
}

fn concurrency_nesting(snapshot: Option<&HelperFileSnapshot>) -> u32 {
    snapshot.map(|value| value.max_nesting).unwrap_or_default()
}

fn has_concurrency_primitives(snapshot: &HelperFileSnapshot) -> bool {
    snapshot.goroutines > 0
        || snapshot.defers > 0
        || snapshot.selects > 0
        || snapshot.sends > 0
        || snapshot.receives > 0
        || snapshot.closes > 0
}

fn error_handling_changed(
    before: Option<&HelperFileSnapshot>,
    after: Option<&HelperFileSnapshot>,
) -> bool {
    let before = before.cloned().unwrap_or(HelperFileSnapshot {
        path: String::new(),
        goroutines: 0,
        defers: 0,
        selects: 0,
        sends: 0,
        receives: 0,
        closes: 0,
        max_nesting: 0,
        errors_is_as_calls: 0,
        nil_checks: 0,
        panic_calls: 0,
        recover_calls: 0,
        context_checks: 0,
        time_calls: 0,
        retry_markers: 0,
        receiver_kinds: HashMap::new(),
        cleanup_calls: 0,
    });
    let after = after.cloned().unwrap_or(HelperFileSnapshot {
        path: String::new(),
        goroutines: 0,
        defers: 0,
        selects: 0,
        sends: 0,
        receives: 0,
        closes: 0,
        max_nesting: 0,
        errors_is_as_calls: 0,
        nil_checks: 0,
        panic_calls: 0,
        recover_calls: 0,
        context_checks: 0,
        time_calls: 0,
        retry_markers: 0,
        receiver_kinds: HashMap::new(),
        cleanup_calls: 0,
    });

    before.errors_is_as_calls != after.errors_is_as_calls
        || before.nil_checks != after.nil_checks
        || before.panic_calls != after.panic_calls
        || before.recover_calls != after.recover_calls
        || before.context_checks != after.context_checks
}

fn runtime_behavior_changed(
    before: Option<&HelperFileSnapshot>,
    after: Option<&HelperFileSnapshot>,
) -> bool {
    let before = before.cloned().unwrap_or(HelperFileSnapshot {
        path: String::new(),
        goroutines: 0,
        defers: 0,
        selects: 0,
        sends: 0,
        receives: 0,
        closes: 0,
        max_nesting: 0,
        errors_is_as_calls: 0,
        nil_checks: 0,
        panic_calls: 0,
        recover_calls: 0,
        context_checks: 0,
        time_calls: 0,
        retry_markers: 0,
        receiver_kinds: HashMap::new(),
        cleanup_calls: 0,
    });
    let after = after.cloned().unwrap_or(HelperFileSnapshot {
        path: String::new(),
        goroutines: 0,
        defers: 0,
        selects: 0,
        sends: 0,
        receives: 0,
        closes: 0,
        max_nesting: 0,
        errors_is_as_calls: 0,
        nil_checks: 0,
        panic_calls: 0,
        recover_calls: 0,
        context_checks: 0,
        time_calls: 0,
        retry_markers: 0,
        receiver_kinds: HashMap::new(),
        cleanup_calls: 0,
    });

    before.time_calls != after.time_calls || before.retry_markers != after.retry_markers
}

fn resource_lifecycle_changed(
    before: Option<&HelperFileSnapshot>,
    after: Option<&HelperFileSnapshot>,
) -> bool {
    let before = before.cloned().unwrap_or(HelperFileSnapshot {
        path: String::new(),
        goroutines: 0,
        defers: 0,
        selects: 0,
        sends: 0,
        receives: 0,
        closes: 0,
        max_nesting: 0,
        errors_is_as_calls: 0,
        nil_checks: 0,
        panic_calls: 0,
        recover_calls: 0,
        context_checks: 0,
        time_calls: 0,
        retry_markers: 0,
        receiver_kinds: HashMap::new(),
        cleanup_calls: 0,
    });
    let after = after.cloned().unwrap_or(HelperFileSnapshot {
        path: String::new(),
        goroutines: 0,
        defers: 0,
        selects: 0,
        sends: 0,
        receives: 0,
        closes: 0,
        max_nesting: 0,
        errors_is_as_calls: 0,
        nil_checks: 0,
        panic_calls: 0,
        recover_calls: 0,
        context_checks: 0,
        time_calls: 0,
        retry_markers: 0,
        receiver_kinds: HashMap::new(),
        cleanup_calls: 0,
    });

    before.cleanup_calls != after.cleanup_calls
}

fn go_test_oracle_changed(file: &crate::ChangedFile) -> bool {
    if !file.is_test_file() {
        return false;
    }

    file.added.iter().chain(file.removed.iter()).any(|line| {
        let trimmed = line.trim();
        trimmed.contains("cmp.Diff(")
            || trimmed.contains("assert.")
            || trimmed.contains("require.")
            || trimmed.contains("t.Fatal(")
            || trimmed.contains("t.Fatalf(")
            || trimmed.contains("t.Error(")
            || trimmed.contains("t.Errorf(")
    })
}

fn go_concurrency_weight(snapshot: Option<&HelperFileSnapshot>) -> u32 {
    match concurrency_nesting(snapshot) {
        depth if depth >= 4 => 35,
        3 => 30,
        2 => 25,
        _ => 20,
    }
}

fn fallback_concurrency_weight(file: &crate::ChangedFile) -> u32 {
    match approximate_go_branch_nesting(file) {
        depth if depth >= 4 => 35,
        3 => 30,
        2 => 25,
        _ => 20,
    }
}

fn approximate_go_branch_nesting(file: &crate::ChangedFile) -> usize {
    let mut current_depth = 0usize;
    let mut max_depth = 0usize;

    for line in &file.added {
        let trimmed = line.trim();

        let closing_braces = trimmed.chars().filter(|ch| *ch == '}').count();
        current_depth = current_depth.saturating_sub(closing_braces);

        if starts_go_branch(trimmed) {
            max_depth = max_depth.max(current_depth + 1);
        }

        let opening_braces = trimmed.chars().filter(|ch| *ch == '{').count();
        current_depth += opening_braces;
    }

    max_depth
}

fn starts_go_branch(trimmed: &str) -> bool {
    [
        "if ",
        "if(",
        "else if",
        "for ",
        "switch ",
        "select ",
        "select{",
        "select {",
    ]
    .iter()
    .any(|keyword| trimmed.starts_with(keyword))
}

fn is_exported_go_declaration(line: &str) -> bool {
    let trimmed = line.trim_start();

    if let Some(rest) = trimmed.strip_prefix("func ") {
        return rest
            .chars()
            .next()
            .is_some_and(|ch| ch.is_ascii_uppercase());
    }

    if let Some(rest) = trimmed.strip_prefix("type ") {
        return rest
            .chars()
            .next()
            .is_some_and(|ch| ch.is_ascii_uppercase());
    }

    false
}
