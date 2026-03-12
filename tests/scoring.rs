use shiwake::{
    AnalysisResult, AnalyzerPlugin, ChangedFile, Confidence, ReasonKind, RuleConfig, ScoreConfig,
    analyze_patch, analyze_patch_with_config,
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

struct BonusPlugin;

impl AnalyzerPlugin for BonusPlugin {
    fn id(&self) -> &'static str {
        "bonus"
    }

    fn supports(&self, file: &ChangedFile) -> bool {
        file.path.ends_with(".rs")
    }

    fn analyze(&self, file: &ChangedFile) -> AnalysisResult {
        AnalysisResult::from_plugin(
            file.path.clone(),
            10,
            Confidence::High,
            vec![ReasonKind::PluginSignal.as_reason(file.path.clone(), 10, "plugin bonus")],
        )
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
            review_recommended_max: 59,
        },
        aggregation: shiwake::AggregationConfig {
            top_file_weight: 1.0,
            secondary_file_weight: 0.33,
            max_score: 100,
        },
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
            review_recommended_max: 15,
        },
        aggregation: shiwake::AggregationConfig {
            top_file_weight: 1.0,
            secondary_file_weight: 0.33,
            max_score: 100,
        },
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
fn score_config_parses_from_toml() {
    let config = ScoreConfig::from_toml(
        r#"
schema_version = 1
scoring_model_version = "custom-v1"

[decision_thresholds]
skip_review_max = 10
review_recommended_max = 50

[aggregation]
top_file_weight = 1.0
secondary_file_weight = 0.5
max_score = 80

[[rules]]
kind = "control_flow_change"
score = 70
"#,
    )
    .expect("config should parse");

    assert_eq!(config.scoring_model_version, "custom-v1");
    assert_eq!(config.decision_thresholds.skip_review_max, 10);
    assert_eq!(config.aggregation.max_score, 80);
    assert_eq!(config.rules.len(), 1);
    assert_eq!(config.rules[0].kind, ReasonKind::ControlFlowChange);
    assert_eq!(config.rules[0].score, 70);
}
