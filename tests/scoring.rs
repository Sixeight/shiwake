use std::{
    fs,
    path::{Path, PathBuf},
    process::Command as ProcessCommand,
    time::{SystemTime, UNIX_EPOCH},
};

use shiwake::{
    AnalysisContext, AnalyzeInput, AnalyzeRequest, AnalyzerPlugin, Confidence, PluginAnalysis,
    PluginFinding, PluginScoreMode, ReasonKind, RuleConfig, ScoreConfig, analyze_patch,
    analyze_patch_with_config, analyze_request, analyze_request_with_config, plugins::go::GoPlugin,
};

fn single_file_patch(old_path: &str, new_path: &str, removed: &[&str], added: &[&str]) -> String {
    let mut patch = format!(
        "diff --git a/{old_path} b/{new_path}\n--- a/{old_path}\n+++ b/{new_path}\n@@ -1,{} +1,{} @@\n",
        removed.len().max(1),
        added.len().max(1),
    );

    for line in removed {
        patch.push('-');
        patch.push_str(line);
        patch.push('\n');
    }

    for line in added {
        patch.push('+');
        patch.push_str(line);
        patch.push('\n');
    }

    patch
}

fn unique_dir(name: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("time should move forward")
        .as_nanos();

    std::env::temp_dir().join(format!("shiwake-{name}-{nanos}"))
}

fn git(repo: &Path, args: &[&str]) -> String {
    let output = ProcessCommand::new("git")
        .current_dir(repo)
        .args(args)
        .output()
        .expect("git should run");

    assert!(
        output.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );

    String::from_utf8(output.stdout)
        .expect("git stdout should be utf8")
        .trim()
        .to_string()
}

fn write_file(path: &Path, contents: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("parent should exist");
    }

    fs::write(path, contents).expect("file should be written");
}

struct BonusPlugin;

impl AnalyzerPlugin for BonusPlugin {
    fn id(&self) -> &'static str {
        "bonus"
    }

    fn analyze(&self, ctx: &AnalysisContext) -> PluginAnalysis {
        let findings = ctx
            .files
            .iter()
            .filter(|file| file.path.ends_with(".rs"))
            .map(|file| PluginFinding {
                path: file.path.clone(),
                kind: ReasonKind::PluginSignal,
                message: String::from("plugin bonus"),
                weight_override: None,
                score_mode: PluginScoreMode::Additive,
            })
            .collect();

        PluginAnalysis::new(Confidence::High, findings)
    }
}

#[test]
fn comment_only_diff_scores_zero() {
    let patch = single_file_patch("src/lib.rs", "src/lib.rs", &["// old"], &["// new"]);

    let report = analyze_patch(&patch, &[]).expect("analysis should succeed");

    assert_eq!(report.score, 0);
    assert_eq!(report.decision.as_str(), "skip_review");
    assert_eq!(report.confidence.as_str(), "high");
}

#[test]
fn public_signature_change_scores_high() {
    let patch = single_file_patch(
        "src/lib.rs",
        "src/lib.rs",
        &["pub fn score(diff: &str) -> i32 {"],
        &["pub fn score(diff: &str, strict: bool) -> i32 {"],
    );

    let report = analyze_patch(&patch, &[]).expect("analysis should succeed");

    assert!(report.score >= 60, "score was {}", report.score);
    assert!(
        report
            .reasons
            .iter()
            .any(|reason| reason.kind == ReasonKind::PublicInterfaceChange)
    );
}

#[test]
fn control_flow_change_scores_high() {
    let patch = single_file_patch(
        "src/lib.rs",
        "src/lib.rs",
        &["let value = compute();"],
        &["if needs_retry() {", "    return compute_retry();", "}"],
    );

    let report = analyze_patch(&patch, &[]).expect("analysis should succeed");

    assert!(report.score >= 55, "score was {}", report.score);
    assert!(
        report
            .reasons
            .iter()
            .any(|reason| reason.kind == ReasonKind::ControlFlowChange)
    );
}

#[test]
fn test_assertion_change_scores_high() {
    let patch = single_file_patch(
        "tests/service_test.go",
        "tests/service_test.go",
        &["assert.Equal(t, 200, status)"],
        &["assert.Equal(t, 500, status)"],
    );

    let report = analyze_patch(&patch, &[]).expect("analysis should succeed");

    assert!(report.score >= 50, "score was {}", report.score);
    assert!(
        report
            .reasons
            .iter()
            .any(|reason| reason.kind == ReasonKind::TestExpectationChange)
    );
}

#[test]
fn rename_like_change_stays_low() {
    let patch = single_file_patch(
        "src/lib.rs",
        "src/lib.rs",
        &["let result = fetch_value(item_id);"],
        &["let output = fetch_value(item_id);"],
    );

    let report = analyze_patch(&patch, &[]).expect("analysis should succeed");

    assert!(report.score <= 25, "score was {}", report.score);
}

#[test]
fn larger_patch_scores_higher_than_small_patch() {
    let small_patch = single_file_patch(
        "src/lib.rs",
        "src/lib.rs",
        &["let status = old_status;"],
        &["let status = new_status;"],
    );
    let large_patch = single_file_patch(
        "src/lib.rs",
        "src/lib.rs",
        &["let status = old_status;"],
        &[
            "let first = compute_first();",
            "let second = compute_second();",
            "let third = compute_third();",
            "let fourth = compute_fourth();",
            "let fifth = compute_fifth();",
            "let sixth = compute_sixth();",
            "let seventh = compute_seventh();",
            "let eighth = compute_eighth();",
            "let ninth = compute_ninth();",
            "let tenth = compute_tenth();",
            "let eleventh = compute_eleventh();",
            "let twelfth = compute_twelfth();",
        ],
    );

    let small_report = analyze_patch(&small_patch, &[]).expect("analysis should succeed");
    let large_report = analyze_patch(&large_patch, &[]).expect("analysis should succeed");

    assert!(
        large_report.score > small_report.score,
        "small={}, large={}",
        small_report.score,
        large_report.score
    );
    assert!(
        large_report
            .reasons
            .iter()
            .any(|reason| reason.kind == ReasonKind::ChangeSize)
    );
}

#[test]
fn large_generic_patch_stays_skip_review() {
    let patch = single_file_patch(
        ".github/workflows/ci.yaml",
        ".github/workflows/ci.yaml",
        &["timeout-minutes: 10"],
        &[
            "timeout-minutes: 20",
            "concurrency:",
            "  group: ci-${{ github.ref }}",
            "  cancel-in-progress: true",
            "permissions:",
            "  contents: read",
            "  pull-requests: write",
            "env:",
            "  FOO: bar",
            "  BAZ: qux",
            "  ENABLE_CACHE: true",
            "  RETRY_COUNT: 3",
        ],
    );

    let report = analyze_patch(&patch, &[]).expect("analysis should succeed");

    assert!(report.score <= 24, "report was {:?}", report);
    assert_eq!(report.decision.as_str(), "skip_review");
}

#[test]
fn multiple_generic_files_stay_skip_review() {
    let patch = format!(
        "{}{}",
        single_file_patch(
            "infra/a.tf",
            "infra/a.tf",
            &["enabled = false"],
            &[
                "enabled = true",
                "labels = {",
                "  env = \"prod\"",
                "  team = \"platform\"",
                "  service = \"db\"",
                "}",
                "timeouts = {",
                "  create = \"30m\"",
                "}",
                "retries = 3",
                "region = \"asia-northeast1\"",
            ],
        ),
        single_file_patch(
            "infra/b.tf",
            "infra/b.tf",
            &["enabled = false"],
            &[
                "enabled = true",
                "labels = {",
                "  env = \"prod\"",
                "  team = \"platform\"",
                "}",
                "timeouts = {",
                "  create = \"30m\"",
                "}",
                "retries = 3",
                "region = \"asia-northeast1\"",
            ],
        ),
    );

    let report = analyze_patch(&patch, &[]).expect("analysis should succeed");

    assert!(report.score <= 24, "report was {:?}", report);
    assert_eq!(report.decision.as_str(), "skip_review");
}

#[test]
fn revision_range_adds_repo_hotspot_signal_for_frequently_touched_file() {
    let repo = unique_dir("history-score");
    fs::create_dir_all(&repo).expect("repo dir should exist");

    git(&repo, &["init"]);
    git(&repo, &["config", "user.name", "Tomohiro"]);
    git(&repo, &["config", "user.email", "tomohiro@example.com"]);
    git(&repo, &["config", "commit.gpgsign", "false"]);

    write_file(
        &repo.join("src/hot.rs"),
        "pub fn hot() -> i32 {\n    1\n}\n",
    );
    write_file(
        &repo.join("src/cold.rs"),
        "pub fn cold() -> i32 {\n    1\n}\n",
    );
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-m", "initial"]);

    for value in ["2", "3", "4", "5"] {
        write_file(
            &repo.join("src/hot.rs"),
            &format!("pub fn hot() -> i32 {{\n    {value}\n}}\n"),
        );
        git(&repo, &["add", "src/hot.rs"]);
        git(&repo, &["commit", "-m", "touch hot"]);
    }

    let base = git(&repo, &["rev-parse", "HEAD"]);

    write_file(
        &repo.join("src/hot.rs"),
        "pub fn hot() -> i32 {\n    if needs_hot_path() {\n        return compute_hot();\n    }\n    0\n}\n",
    );
    write_file(
        &repo.join("src/cold.rs"),
        "pub fn cold() -> i32 {\n    let next = compute_cold();\n    next\n}\n",
    );
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-m", "change both"]);
    let head = git(&repo, &["rev-parse", "HEAD"]);

    let report = analyze_request(
        &AnalyzeRequest {
            input: AnalyzeInput::GitRevisionRange {
                repo_root: repo.clone(),
                base,
                head,
            },
            repo_root: Some(repo.clone()),
        },
        &[],
    )
    .expect("analysis should succeed");

    let hot_reason = report
        .reasons
        .iter()
        .find(|reason| reason.kind == ReasonKind::RepoHotspot && reason.file == "src/hot.rs");
    let cold_reason = report
        .reasons
        .iter()
        .find(|reason| reason.kind == ReasonKind::RepoHotspot && reason.file == "src/cold.rs");

    assert!(hot_reason.is_some(), "reasons were {:?}", report.reasons);
    assert!(cold_reason.is_none(), "reasons were {:?}", report.reasons);
}

#[test]
fn large_test_patch_gets_smaller_size_bump_than_production_patch() {
    let production_patch = single_file_patch(
        "src/lib.rs",
        "src/lib.rs",
        &["let status = old_status;"],
        &[
            "let first = compute_first();",
            "let second = compute_second();",
            "let third = compute_third();",
            "let fourth = compute_fourth();",
            "let fifth = compute_fifth();",
            "let sixth = compute_sixth();",
            "let seventh = compute_seventh();",
            "let eighth = compute_eighth();",
            "let ninth = compute_ninth();",
            "let tenth = compute_tenth();",
            "let eleventh = compute_eleventh();",
            "let twelfth = compute_twelfth();",
        ],
    );
    let test_patch = single_file_patch(
        "tests/lib_test.rs",
        "tests/lib_test.rs",
        &["let status = old_status;"],
        &[
            "let first = compute_first();",
            "let second = compute_second();",
            "let third = compute_third();",
            "let fourth = compute_fourth();",
            "let fifth = compute_fifth();",
            "let sixth = compute_sixth();",
            "let seventh = compute_seventh();",
            "let eighth = compute_eighth();",
            "let ninth = compute_ninth();",
            "let tenth = compute_tenth();",
            "let eleventh = compute_eleventh();",
            "let twelfth = compute_twelfth();",
        ],
    );

    let production_report = analyze_patch(&production_patch, &[]).expect("analysis should succeed");
    let test_report = analyze_patch(&test_patch, &[]).expect("analysis should succeed");

    assert!(
        production_report.score > test_report.score,
        "production={}, test={}",
        production_report.score,
        test_report.score
    );
}

#[test]
fn repo_hotspot_does_not_raise_generic_change_without_semantic_risk() {
    let repo = unique_dir("history-generic");
    fs::create_dir_all(&repo).expect("repo dir should exist");

    git(&repo, &["init"]);
    git(&repo, &["config", "user.name", "Tomohiro"]);
    git(&repo, &["config", "user.email", "tomohiro@example.com"]);
    git(&repo, &["config", "commit.gpgsign", "false"]);

    write_file(
        &repo.join("src/hot.rs"),
        "pub fn hot() -> i32 {\n    1\n}\n",
    );
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-m", "initial"]);

    for value in ["2", "3", "4", "5"] {
        write_file(
            &repo.join("src/hot.rs"),
            &format!("pub fn hot() -> i32 {{\n    {value}\n}}\n"),
        );
        git(&repo, &["add", "src/hot.rs"]);
        git(&repo, &["commit", "-m", "touch hot"]);
    }

    let base = git(&repo, &["rev-parse", "HEAD"]);

    write_file(
        &repo.join("src/hot.rs"),
        "pub fn hot() -> i32 {\n    let next = compute_hot();\n    next\n}\n",
    );
    git(&repo, &["add", "src/hot.rs"]);
    git(&repo, &["commit", "-m", "generic change"]);
    let head = git(&repo, &["rev-parse", "HEAD"]);

    let report = analyze_request(
        &AnalyzeRequest {
            input: AnalyzeInput::GitRevisionRange {
                repo_root: repo.clone(),
                base,
                head,
            },
            repo_root: Some(repo.clone()),
        },
        &[],
    )
    .expect("analysis should succeed");

    assert!(
        !report
            .reasons
            .iter()
            .any(|reason| reason.kind == ReasonKind::RepoHotspot),
        "reasons were {:?}",
        report.reasons
    );
}

#[test]
fn generated_files_from_gitattributes_are_excluded_from_scoring() {
    let repo = unique_dir("generated-filter");
    fs::create_dir_all(&repo).expect("repo dir should exist");

    git(&repo, &["init"]);
    git(&repo, &["config", "user.name", "Tomohiro"]);
    git(&repo, &["config", "user.email", "tomohiro@example.com"]);
    git(&repo, &["config", "commit.gpgsign", "false"]);

    write_file(
        &repo.join(".gitattributes"),
        "gen/** linguist-generated=true\n",
    );
    write_file(
        &repo.join("gen/output.go"),
        "package gen\n\nfunc Build() int {\n    return 1\n}\n",
    );
    write_file(
        &repo.join("src/lib.rs"),
        "pub fn score() -> i32 {\n    1\n}\n",
    );
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-m", "initial"]);
    let base = git(&repo, &["rev-parse", "HEAD"]);

    write_file(
        &repo.join("gen/output.go"),
        "package gen\n\nfunc Build() int {\n    if useNewFlow() {\n        return 2\n    }\n    return 1\n}\n",
    );
    write_file(
        &repo.join("src/lib.rs"),
        "pub fn score() -> i32 {\n    let next = compute_score();\n    next\n}\n",
    );
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-m", "update"]);
    let head = git(&repo, &["rev-parse", "HEAD"]);

    let report = analyze_request(
        &AnalyzeRequest {
            input: AnalyzeInput::GitRevisionRange {
                repo_root: repo.clone(),
                base,
                head,
            },
            repo_root: Some(repo.clone()),
        },
        &[&GoPlugin],
    )
    .expect("analysis should succeed");

    assert!(
        report
            .by_file
            .iter()
            .all(|file| file.path != "gen/output.go"),
        "by_file was {:?}",
        report.by_file
    );
    assert!(
        report
            .reasons
            .iter()
            .all(|reason| reason.file != "gen/output.go"),
        "reasons were {:?}",
        report.reasons
    );
    assert_eq!(report.by_file.len(), 1, "by_file was {:?}", report.by_file);
}

#[test]
fn generated_files_are_excluded_for_patch_input_when_repo_root_is_known() {
    let repo = unique_dir("generated-filter-patch");
    fs::create_dir_all(&repo).expect("repo dir should exist");
    write_file(
        &repo.join(".gitattributes"),
        "gen/** linguist-generated=true\n",
    );

    let patch = single_file_patch(
        "gen/output.go",
        "gen/output.go",
        &["func Build() int {", "    return 1", "}"],
        &[
            "func Build() int {",
            "    if useNewFlow() {",
            "        return 2",
            "    }",
            "    return 1",
            "}",
        ],
    );

    let report = analyze_request(
        &AnalyzeRequest {
            input: AnalyzeInput::PatchText { patch },
            repo_root: Some(repo),
        },
        &[&GoPlugin],
    )
    .expect("analysis should succeed");

    assert_eq!(report.score, 0, "report was {:?}", report);
    assert!(
        report.by_file.is_empty(),
        "by_file was {:?}",
        report.by_file
    );
    assert!(
        report.reasons.is_empty(),
        "reasons were {:?}",
        report.reasons
    );
}

#[test]
fn generated_file_attributes_can_be_customized_in_config() {
    let repo = unique_dir("custom-generated-filter");
    fs::create_dir_all(&repo).expect("repo dir should exist");

    git(&repo, &["init"]);
    git(&repo, &["config", "user.name", "Tomohiro"]);
    git(&repo, &["config", "user.email", "tomohiro@example.com"]);
    git(&repo, &["config", "commit.gpgsign", "false"]);

    write_file(
        &repo.join(".gitattributes"),
        "gen/** pr-review-skip\nraw/** linguist-generated=true\n",
    );
    write_file(
        &repo.join("gen/output.go"),
        "package gen\n\nfunc Build() int {\n    return 1\n}\n",
    );
    write_file(
        &repo.join("raw/output.go"),
        "package raw\n\nfunc Build() int {\n    return 1\n}\n",
    );
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-m", "initial"]);
    let base = git(&repo, &["rev-parse", "HEAD"]);

    write_file(
        &repo.join("gen/output.go"),
        "package gen\n\nfunc Build() int {\n    if useNewFlow() {\n        return 2\n    }\n    return 1\n}\n",
    );
    write_file(
        &repo.join("raw/output.go"),
        "package raw\n\nfunc Build() int {\n    if useNewFlow() {\n        return 2\n    }\n    return 1\n}\n",
    );
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-m", "update"]);
    let head = git(&repo, &["rev-parse", "HEAD"]);

    let config = ScoreConfig::from_toml(
        r#"
schema_version = 1
scoring_model_version = "custom-v1"
gitattributes_skip_attributes = ["pr-review-skip"]

[decision_thresholds]
skip_review_max = 24
review_optional_max = 29
review_suggested_max = 39
review_recommended_max = 59

[aggregation]
max_score = 100
secondary_ratio = 0.2
secondary_cap = 12

[[rules]]
kind = "control_flow_change"
score = 65
"#,
    )
    .expect("config should parse");

    let report = analyze_request_with_config(
        &AnalyzeRequest {
            input: AnalyzeInput::GitRevisionRange {
                repo_root: repo.clone(),
                base,
                head,
            },
            repo_root: Some(repo),
        },
        &[&GoPlugin],
        &config,
    )
    .expect("analysis should succeed");

    assert!(
        report
            .by_file
            .iter()
            .all(|file| file.path != "gen/output.go"),
        "by_file was {:?}",
        report.by_file
    );
    assert!(
        report
            .by_file
            .iter()
            .any(|file| file.path == "raw/output.go"),
        "by_file was {:?}",
        report.by_file
    );
}

#[test]
fn medium_semantic_change_with_hotspot_stays_recommended() {
    let repo = unique_dir("history-medium-semantic");
    fs::create_dir_all(&repo).expect("repo dir should exist");

    git(&repo, &["init"]);
    git(&repo, &["config", "user.name", "Tomohiro"]);
    git(&repo, &["config", "user.email", "tomohiro@example.com"]);
    git(&repo, &["config", "commit.gpgsign", "false"]);

    write_file(
        &repo.join("src/hot.rs"),
        "pub fn hot() -> i32 {\n    1\n}\n",
    );
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-m", "initial"]);

    for value in ["2", "3", "4", "5", "6", "7", "8", "9"] {
        write_file(
            &repo.join("src/hot.rs"),
            &format!("pub fn hot() -> i32 {{\n    {value}\n}}\n"),
        );
        git(&repo, &["add", "src/hot.rs"]);
        git(&repo, &["commit", "-m", "touch hot"]);
    }

    let base = git(&repo, &["rev-parse", "HEAD"]);

    write_file(
        &repo.join("src/hot.rs"),
        "pub fn hot() -> i32 {\n    let mut value = 0;\n    if should_use_override() {\n        value = compute_override();\n    }\n    value\n}\n",
    );
    git(&repo, &["add", "src/hot.rs"]);
    git(&repo, &["commit", "-m", "semantic change"]);
    let head = git(&repo, &["rev-parse", "HEAD"]);

    let report = analyze_request(
        &AnalyzeRequest {
            input: AnalyzeInput::GitRevisionRange {
                repo_root: repo.clone(),
                base,
                head,
            },
            repo_root: Some(repo),
        },
        &[],
    )
    .expect("analysis should succeed");

    assert!(report.score < 60, "report was {:?}", report);
    assert_eq!(report.decision.as_str(), "review_recommended");
}

#[test]
fn semantic_change_stays_above_large_generic_change() {
    let generic_patch = single_file_patch(
        "src/lib.rs",
        "src/lib.rs",
        &["let status = old_status;"],
        &[
            "let first = compute_first();",
            "let second = compute_second();",
            "let third = compute_third();",
            "let fourth = compute_fourth();",
            "let fifth = compute_fifth();",
            "let sixth = compute_sixth();",
            "let seventh = compute_seventh();",
            "let eighth = compute_eighth();",
            "let ninth = compute_ninth();",
            "let tenth = compute_tenth();",
            "let eleventh = compute_eleventh();",
            "let twelfth = compute_twelfth();",
        ],
    );
    let semantic_patch = single_file_patch(
        "src/lib.rs",
        "src/lib.rs",
        &["let value = compute();"],
        &["if needs_retry() {", "    return compute_retry();", "}"],
    );

    let generic_report = analyze_patch(&generic_patch, &[]).expect("analysis should succeed");
    let semantic_report = analyze_patch(&semantic_patch, &[]).expect("analysis should succeed");

    assert!(
        semantic_report.score > generic_report.score,
        "generic={}, semantic={}",
        generic_report.score,
        semantic_report.score
    );
}

#[test]
fn secondary_files_do_not_overwhelm_the_highest_risk_file() {
    let patch = format!(
        "{}{}{}{}",
        single_file_patch(
            "src/api.rs",
            "src/api.rs",
            &["pub fn run() -> i32 {"],
            &["pub fn run(strict: bool) -> i32 {"],
        ),
        single_file_patch(
            "src/a.rs",
            "src/a.rs",
            &["let status = old_status;"],
            &[
                "let first = compute_first();",
                "let second = compute_second();",
                "let third = compute_third();",
                "let fourth = compute_fourth();",
                "let fifth = compute_fifth();",
                "let sixth = compute_sixth();",
            ],
        ),
        single_file_patch(
            "src/b.rs",
            "src/b.rs",
            &["let status = old_status;"],
            &[
                "let first = compute_first();",
                "let second = compute_second();",
                "let third = compute_third();",
                "let fourth = compute_fourth();",
                "let fifth = compute_fifth();",
                "let sixth = compute_sixth();",
            ],
        ),
        single_file_patch(
            "src/c.rs",
            "src/c.rs",
            &["let status = old_status;"],
            &[
                "let first = compute_first();",
                "let second = compute_second();",
                "let third = compute_third();",
                "let fourth = compute_fourth();",
                "let fifth = compute_fifth();",
                "let sixth = compute_sixth();",
            ],
        ),
    );

    let report = analyze_patch(&patch, &[]).expect("analysis should succeed");

    assert!(report.score < 95, "score was {}", report.score);
}

#[test]
fn plugin_can_add_signal() {
    let patch = single_file_patch(
        "src/lib.rs",
        "src/lib.rs",
        &["let result = fetch_value(item_id);"],
        &["let output = fetch_value(item_id);"],
    );

    let plugin = BonusPlugin;
    let report = analyze_patch(&patch, &[&plugin]).expect("analysis should succeed");

    assert!(report.score >= 10, "score was {}", report.score);
    assert!(
        report
            .reasons
            .iter()
            .any(|reason| reason.kind == ReasonKind::PluginSignal)
    );
}

#[test]
fn config_can_lower_public_interface_weight() {
    let patch = single_file_patch(
        "src/lib.rs",
        "src/lib.rs",
        &["pub fn score(diff: &str) -> i32 {"],
        &["pub fn score(diff: &str, strict: bool) -> i32 {"],
    );
    let config = ScoreConfig {
        schema_version: 1,
        scoring_model_version: "custom-v1".to_string(),
        decision_thresholds: shiwake::DecisionThresholds {
            skip_review_max: 24,
            review_optional_max: 29,
            review_suggested_max: 39,
            review_recommended_max: 59,
        },
        aggregation: shiwake::AggregationConfig {
            max_score: 100,
            secondary_ratio: 0.2,
            secondary_cap: 12,
        },
        gitattributes_skip_attributes: vec![String::from("linguist-generated")],
        rules: vec![RuleConfig {
            kind: ReasonKind::PublicInterfaceChange,
            score: 40,
        }],
    };

    let report = analyze_patch_with_config(&patch, &[], &config).expect("analysis should succeed");

    assert_eq!(report.score, 40);
    assert_eq!(report.decision.as_str(), "review_recommended");
}

#[test]
fn config_can_change_decision_thresholds() {
    let patch = single_file_patch(
        "src/lib.rs",
        "src/lib.rs",
        &["let value = compute();"],
        &["let output = compute_again();"],
    );
    let config = ScoreConfig {
        schema_version: 1,
        scoring_model_version: "custom-v1".to_string(),
        decision_thresholds: shiwake::DecisionThresholds {
            skip_review_max: 10,
            review_optional_max: 15,
            review_suggested_max: 18,
            review_recommended_max: 19,
        },
        aggregation: shiwake::AggregationConfig {
            max_score: 100,
            secondary_ratio: 0.2,
            secondary_cap: 12,
        },
        gitattributes_skip_attributes: vec![String::from("linguist-generated")],
        rules: vec![RuleConfig {
            kind: ReasonKind::GenericCodeChange,
            score: 20,
        }],
    };

    let report = analyze_patch_with_config(&patch, &[], &config).expect("analysis should succeed");

    assert_eq!(report.score, 20);
    assert_eq!(report.decision.as_str(), "review_required");
}

#[test]
fn decision_uses_intermediate_bands() {
    let patch = single_file_patch(
        "src/lib.rs",
        "src/lib.rs",
        &["pub fn score(diff: &str) -> i32 {"],
        &["pub fn score(diff: &str, strict: bool) -> i32 {"],
    );
    let optional_config = ScoreConfig {
        schema_version: 1,
        scoring_model_version: "custom-v1".to_string(),
        decision_thresholds: shiwake::DecisionThresholds {
            skip_review_max: 24,
            review_optional_max: 29,
            review_suggested_max: 39,
            review_recommended_max: 59,
        },
        aggregation: shiwake::AggregationConfig {
            max_score: 100,
            secondary_ratio: 0.2,
            secondary_cap: 12,
        },
        gitattributes_skip_attributes: vec![String::from("linguist-generated")],
        rules: vec![RuleConfig {
            kind: ReasonKind::PublicInterfaceChange,
            score: 28,
        }],
    };
    let suggested_config = ScoreConfig {
        schema_version: 1,
        scoring_model_version: "custom-v1".to_string(),
        decision_thresholds: shiwake::DecisionThresholds {
            skip_review_max: 24,
            review_optional_max: 29,
            review_suggested_max: 39,
            review_recommended_max: 59,
        },
        aggregation: shiwake::AggregationConfig {
            max_score: 100,
            secondary_ratio: 0.2,
            secondary_cap: 12,
        },
        gitattributes_skip_attributes: vec![String::from("linguist-generated")],
        rules: vec![RuleConfig {
            kind: ReasonKind::PublicInterfaceChange,
            score: 39,
        }],
    };

    let optional_report =
        analyze_patch_with_config(&patch, &[], &optional_config).expect("analysis should succeed");
    let suggested_report =
        analyze_patch_with_config(&patch, &[], &suggested_config).expect("analysis should succeed");

    assert_eq!(optional_report.score, 28);
    assert_eq!(optional_report.decision.as_str(), "review_optional");
    assert_eq!(suggested_report.score, 39);
    assert_eq!(suggested_report.decision.as_str(), "review_suggested");
}

#[test]
fn score_config_parses_from_toml() {
    let config = ScoreConfig::from_toml(
        r#"
schema_version = 1
scoring_model_version = "custom-v1"
gitattributes_skip_attributes = ["linguist-generated", "pr-review-skip"]

[decision_thresholds]
skip_review_max = 10
review_optional_max = 20
review_suggested_max = 35
review_recommended_max = 50

[aggregation]
max_score = 80
secondary_ratio = 0.2
secondary_cap = 10

[[rules]]
kind = "control_flow_change"
score = 70
"#,
    )
    .expect("config should parse");

    assert_eq!(config.scoring_model_version, "custom-v1");
    assert_eq!(config.decision_thresholds.skip_review_max, 10);
    assert_eq!(config.decision_thresholds.review_optional_max, 20);
    assert_eq!(config.decision_thresholds.review_suggested_max, 35);
    assert_eq!(config.aggregation.max_score, 80);
    assert_eq!(config.aggregation.secondary_cap, 10);
    assert_eq!(
        config.gitattributes_skip_attributes,
        vec![
            String::from("linguist-generated"),
            String::from("pr-review-skip")
        ]
    );
    assert_eq!(config.rules.len(), 1);
    assert_eq!(config.rules[0].kind, ReasonKind::ControlFlowChange);
    assert_eq!(config.rules[0].score, 70);
}

#[test]
fn go_plugin_adds_signal_for_select_statements() {
    let patch = single_file_patch(
        "internal/service.go",
        "internal/service.go",
        &["return run(ctx)"],
        &[
            "select {",
            "case <-ctx.Done():",
            "    return ctx.Err()",
            "default:",
            "    return run(ctx)",
            "}",
        ],
    );

    let plugin = GoPlugin::new();
    let report = analyze_patch(&patch, &[&plugin]).expect("analysis should succeed");

    assert!(
        report
            .reasons
            .iter()
            .any(|reason| reason.kind == ReasonKind::GoConcurrencyChange
                && reason.message.contains("go select")),
    );
    assert_eq!(report.confidence.as_str(), "medium");
    assert!(report.score >= 65, "score was {}", report.score);
}

#[test]
fn go_plugin_adds_signal_for_exported_api_changes() {
    let patch = single_file_patch(
        "pkg/api.go",
        "pkg/api.go",
        &["func Build(ctx context.Context) error {"],
        &["func Build(ctx context.Context, strict bool) error {"],
    );

    let plugin = GoPlugin::new();
    let report = analyze_patch(&patch, &[&plugin]).expect("analysis should succeed");

    assert!(
        report
            .reasons
            .iter()
            .any(|reason| reason.kind == ReasonKind::GoExportedApiChange
                && reason.message.contains("exported go api")),
    );
    assert_eq!(report.confidence.as_str(), "medium");
    assert!(report.score >= 70, "score was {}", report.score);
}

#[test]
fn go_test_file_changes_are_scored_lower_than_production_logic_changes() {
    let patch = format!(
        "{}{}",
        single_file_patch(
            "server/component/ride-dispatch/e2etest/graphql_unkan_query_notifications_test.go",
            "server/component/ride-dispatch/e2etest/graphql_unkan_query_notifications_test.go",
            &[
                "func TestNotifications(t *testing.T) {}",
                "return existingNotification"
            ],
            &[
                "func TestNotifications_NotificationLevel(t *testing.T) {",
                "if got != want {",
                "    t.Fatal(diff)",
                "}",
                "}",
            ],
        ),
        single_file_patch(
            "server/component/ride-dispatch/internal/domain/dispatch/dispatch.go",
            "server/component/ride-dispatch/internal/domain/dispatch/dispatch.go",
            &["message := NotificationMessageScheduledRidePreNotification"],
            &[
                "message := NotificationMessageScheduledRidePreNotification",
                "if pref.SkipPickupLocationRegionCheck && !pref.ExternalTaxiVehicleID.Valid {",
                "    message = NotificationMessageScheduledRidePreNotificationNominationRequired",
                "}",
            ],
        )
    );

    let report = analyze_patch(&patch, &[]).expect("analysis should succeed");
    let test_file_reason_kinds: Vec<_> = report
        .reasons
        .iter()
        .filter(|reason| {
            reason
                .file
                .ends_with("graphql_unkan_query_notifications_test.go")
        })
        .map(|reason| reason.kind.clone())
        .collect();

    assert!(
        !test_file_reason_kinds.contains(&ReasonKind::PublicInterfaceChange),
        "test file should not be treated as public interface: {test_file_reason_kinds:?}"
    );
    assert!(report.score < 90, "score was {}", report.score);
}

#[test]
fn local_assignment_branch_scores_lower_than_return_branch() {
    let light_patch = single_file_patch(
        "server/component/ride-dispatch/internal/domain/dispatch/dispatch.go",
        "server/component/ride-dispatch/internal/domain/dispatch/dispatch.go",
        &["message := NotificationMessageScheduledRidePreNotification"],
        &[
            "message := NotificationMessageScheduledRidePreNotification",
            "if pref.SkipPickupLocationRegionCheck && !pref.ExternalTaxiVehicleID.Valid {",
            "    message = NotificationMessageScheduledRidePreNotificationNominationRequired",
            "}",
        ],
    );
    let heavy_patch = single_file_patch(
        "server/component/ride-dispatch/internal/domain/dispatch/dispatch.go",
        "server/component/ride-dispatch/internal/domain/dispatch/dispatch.go",
        &["return message"],
        &[
            "if pref.SkipPickupLocationRegionCheck && !pref.ExternalTaxiVehicleID.Valid {",
            "    return NotificationMessageScheduledRidePreNotificationNominationRequired",
            "}",
            "return message",
        ],
    );

    let light_report = analyze_patch(&light_patch, &[]).expect("analysis should succeed");
    let heavy_report = analyze_patch(&heavy_patch, &[]).expect("analysis should succeed");

    let light_reason = light_report
        .reasons
        .iter()
        .find(|reason| reason.kind == ReasonKind::ControlFlowChange)
        .expect("light patch should have control flow reason");
    let heavy_reason = heavy_report
        .reasons
        .iter()
        .find(|reason| reason.kind == ReasonKind::ControlFlowChange)
        .expect("heavy patch should have control flow reason");

    assert!(light_reason.weight < heavy_reason.weight);
    assert!(light_report.score < heavy_report.score);
}

#[test]
fn deeper_nested_branch_scores_higher_than_shallow_branch() {
    let shallow_patch = single_file_patch(
        "server/component/ride-dispatch/internal/domain/dispatch/dispatch.go",
        "server/component/ride-dispatch/internal/domain/dispatch/dispatch.go",
        &["message := NotificationMessageScheduledRidePreNotification"],
        &[
            "message := NotificationMessageScheduledRidePreNotification",
            "if pref.SkipPickupLocationRegionCheck {",
            "    message = NotificationMessageScheduledRidePreNotificationNominationRequired",
            "}",
        ],
    );
    let deep_patch = single_file_patch(
        "server/component/ride-dispatch/internal/domain/dispatch/dispatch.go",
        "server/component/ride-dispatch/internal/domain/dispatch/dispatch.go",
        &["message := NotificationMessageScheduledRidePreNotification"],
        &[
            "message := NotificationMessageScheduledRidePreNotification",
            "if pref.SkipPickupLocationRegionCheck {",
            "    if !pref.ExternalTaxiVehicleID.Valid {",
            "        message = NotificationMessageScheduledRidePreNotificationNominationRequired",
            "    }",
            "}",
        ],
    );

    let shallow_report = analyze_patch(&shallow_patch, &[]).expect("analysis should succeed");
    let deep_report = analyze_patch(&deep_patch, &[]).expect("analysis should succeed");

    let shallow_reason = shallow_report
        .reasons
        .iter()
        .find(|reason| reason.kind == ReasonKind::ControlFlowChange)
        .expect("shallow patch should have control flow reason");
    let deep_reason = deep_report
        .reasons
        .iter()
        .find(|reason| reason.kind == ReasonKind::ControlFlowChange)
        .expect("deep patch should have control flow reason");

    assert!(shallow_reason.weight < deep_reason.weight);
    assert!(shallow_report.score < deep_report.score);
}
