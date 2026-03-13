use std::{
    fs,
    time::{SystemTime, UNIX_EPOCH},
};

use assert_cmd::Command;

fn unique_path(name: &str) -> std::path::PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("time should move forward")
        .as_nanos();

    std::env::temp_dir().join(format!("shiwake-{name}-{nanos}.patch"))
}

#[test]
fn cli_prints_json_report_for_patch_input() {
    let patch_path = unique_path("cli");
    let patch = "\
diff --git a/src/lib.rs b/src/lib.rs
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -1,1 +1,1 @@
-pub fn score(diff: &str) -> i32 {
+pub fn score(diff: &str, strict: bool) -> i32 {
";

    fs::write(&patch_path, patch).expect("patch should be written");

    let assert = Command::cargo_bin("shiwake")
        .expect("binary should build")
        .args(["--repo", ".", "--patch"])
        .arg(&patch_path)
        .assert();

    let output = String::from_utf8(assert.get_output().stdout.clone()).expect("stdout is utf8");

    assert!(output.contains("\"score\""), "stdout was {output}");
    assert!(output.contains("\"decision\""), "stdout was {output}");

    fs::remove_file(patch_path).expect("patch should be removed");
}

#[test]
fn cli_uses_custom_config_for_thresholds() {
    let patch_path = unique_path("cli-patch");
    let config_path = unique_path("cli-config");
    let patch = "\
diff --git a/src/lib.rs b/src/lib.rs
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -1,1 +1,1 @@
-pub fn score(diff: &str) -> i32 {
+pub fn score(diff: &str, strict: bool) -> i32 {
";
    let config = r#"
schema_version = 1
scoring_model_version = "custom-v1"

[decision_thresholds]
skip_review_max = 10
review_recommended_max = 30

[aggregation]
max_score = 100
secondary_ratio = 0.2
secondary_cap = 12

[[rules]]
kind = "comment_only"
score = 0

[[rules]]
kind = "import_only"
score = 5

[[rules]]
kind = "refactor_like_change"
score = 10

[[rules]]
kind = "public_interface_change"
score = 20

[[rules]]
kind = "control_flow_change"
score = 65

[[rules]]
kind = "test_expectation_change"
score = 55

[[rules]]
kind = "generic_code_change"
score = 20

[[rules]]
kind = "plugin_signal"
score = 10
"#;

    fs::write(&patch_path, patch).expect("patch should be written");
    fs::write(&config_path, config).expect("config should be written");

    let assert = Command::cargo_bin("shiwake")
        .expect("binary should build")
        .args(["--repo", ".", "--patch"])
        .arg(&patch_path)
        .args(["--config"])
        .arg(&config_path)
        .assert();

    let output = String::from_utf8(assert.get_output().stdout.clone()).expect("stdout is utf8");

    assert!(
        output.contains("\"scoring_model_version\":\"custom-v1\""),
        "stdout was {output}"
    );
    assert!(
        output.contains("\"decision\":\"review_recommended\""),
        "stdout was {output}"
    );

    fs::remove_file(patch_path).expect("patch should be removed");
    fs::remove_file(config_path).expect("config should be removed");
}
