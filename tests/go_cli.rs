use std::{
    fs,
    path::{Path, PathBuf},
    process::Command as ProcessCommand,
    time::{SystemTime, UNIX_EPOCH},
};

use assert_cmd::Command;

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

fn init_go_repo(name: &str, initial_main: &str, updated_main: &str) -> (PathBuf, String, String) {
    let repo = unique_dir(name);
    fs::create_dir_all(&repo).expect("repo dir should exist");

    git(&repo, &["init"]);
    git(&repo, &["config", "user.name", "Tomohiro"]);
    git(&repo, &["config", "user.email", "tomohiro@example.com"]);
    git(&repo, &["config", "commit.gpgsign", "false"]);

    write_file(
        &repo.join("go.mod"),
        "module example.com/shiwake-test\n\ngo 1.26.0\n",
    );
    write_file(&repo.join("main.go"), initial_main);
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-m", "initial"]);
    let base = git(&repo, &["rev-parse", "HEAD"]);

    write_file(&repo.join("main.go"), updated_main);
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-m", "update"]);
    let head = git(&repo, &["rev-parse", "HEAD"]);

    (repo, base, head)
}

#[test]
fn go_plugin_detects_exported_api_change_from_revision_range() {
    let initial = r#"package main

import "context"

type Runner interface {
    Run(context.Context) error
}

type worker struct{}

func (worker) Run(ctx context.Context) error {
    return nil
}

func Build(ctx context.Context) error {
    return worker{}.Run(ctx)
}
"#;
    let updated = r#"package main

import "context"

type Runner interface {
    Run(context.Context) error
}

type worker struct{}

func (worker) Run(ctx context.Context, strict bool) error {
    return nil
}

func Build(ctx context.Context, strict bool) error {
    return worker{}.Run(ctx, strict)
}
"#;
    let (repo, base, head) = init_go_repo("go-v2-api", initial, updated);

    let assert = Command::cargo_bin("shiwake")
        .expect("binary should build")
        .args([
            "--repo",
            repo.to_str().expect("repo path should be utf8"),
            "--base",
            &base,
            "--head",
            &head,
            "--plugin",
            "go",
        ])
        .assert();

    let output = String::from_utf8(assert.get_output().stdout.clone()).expect("stdout is utf8");

    assert!(
        output.contains("\"kind\":\"go_exported_api_change\""),
        "stdout was {output}"
    );
    assert!(
        output.contains("\"kind\":\"go_interface_break\""),
        "stdout was {output}"
    );
}

#[test]
fn go_plugin_detects_concurrency_change_from_revision_range() {
    let initial = r#"package main

import "context"

func Run(ctx context.Context) error {
    return nil
}
"#;
    let updated = r#"package main

import "context"

func Run(ctx context.Context) error {
    select {
    case <-ctx.Done():
        return ctx.Err()
    default:
        return nil
    }
}
"#;
    let (repo, base, head) = init_go_repo("go-v2-concurrency", initial, updated);

    let assert = Command::cargo_bin("shiwake")
        .expect("binary should build")
        .args([
            "--repo",
            repo.to_str().expect("repo path should be utf8"),
            "--base",
            &base,
            "--head",
            &head,
            "--plugin",
            "go",
        ])
        .assert();

    let output = String::from_utf8(assert.get_output().stdout.clone()).expect("stdout is utf8");

    assert!(
        output.contains("\"kind\":\"go_concurrency_change\""),
        "stdout was {output}"
    );
}

#[test]
fn go_plugin_falls_back_for_patch_input() {
    let initial = r#"package main

import "context"

func Build(ctx context.Context) error {
    return nil
}
"#;
    let updated = r#"package main

import "context"

func Build(ctx context.Context, strict bool) error {
    return nil
}
"#;
    let (repo, base, head) = init_go_repo("go-v2-fallback", initial, updated);
    let patch = git(&repo, &["diff", &base, &head]);
    let patch_path = unique_dir("go-v2-fallback-patch");
    write_file(&patch_path, &patch);

    let assert = Command::cargo_bin("shiwake")
        .expect("binary should build")
        .args([
            "--repo",
            repo.to_str().expect("repo path should be utf8"),
            "--patch",
            patch_path.to_str().expect("patch path should be utf8"),
            "--plugin",
            "go",
        ])
        .assert();

    let output = String::from_utf8(assert.get_output().stdout.clone()).expect("stdout is utf8");

    assert!(
        output.contains("\"kind\":\"go_analysis_fallback\""),
        "stdout was {output}"
    );
    assert!(
        output.contains("\"confidence\":\"medium\""),
        "stdout was {output}"
    );
}

#[test]
fn go_plugin_handles_module_imports_without_fallback() {
    let initial = r#"package main

import (
    "context"
    "github.com/google/uuid"
)

type Runner interface {
    Run(context.Context) error
}

type worker struct{}

func (worker) Run(ctx context.Context) error {
    _ = uuid.Nil
    return nil
}

func Build(ctx context.Context) error {
    return worker{}.Run(ctx)
}
"#;
    let updated = r#"package main

import (
    "context"
    "github.com/google/uuid"
)

type Runner interface {
    Run(context.Context) error
}

type worker struct{}

func (worker) Run(ctx context.Context, strict bool) error {
    _ = uuid.Nil
    return nil
}

func Build(ctx context.Context, strict bool) error {
    return worker{}.Run(ctx, strict)
}
"#;
    let (repo, base, head) = init_go_repo("go-module-import", initial, updated);

    let assert = Command::cargo_bin("shiwake")
        .expect("binary should build")
        .args([
            "--repo",
            repo.to_str().expect("repo path should be utf8"),
            "--base",
            &base,
            "--head",
            &head,
            "--plugin",
            "go",
        ])
        .assert();

    let output = String::from_utf8(assert.get_output().stdout.clone()).expect("stdout is utf8");

    assert!(
        !output.contains("\"kind\":\"go_analysis_fallback\""),
        "stdout was {output}"
    );
    assert!(
        output.contains("\"kind\":\"go_interface_break\""),
        "stdout was {output}"
    );
}

#[test]
fn go_plugin_scores_deeper_concurrency_nesting_higher() {
    let shallow = r#"package main

import "context"

func Run(ctx context.Context, ch chan int) error {
    select {
    case <-ctx.Done():
        return ctx.Err()
    default:
        return nil
    }
}
"#;
    let deep = r#"package main

import "context"

func Run(ctx context.Context, ch chan int) error {
    select {
    case <-ctx.Done():
        if len(ch) > 0 {
            select {
            case <-ctx.Done():
                return ctx.Err()
            default:
                return nil
            }
        }
        return ctx.Err()
    default:
        return nil
    }
}
"#;
    let (repo, base, head) = init_go_repo("go-nesting", shallow, deep);

    let assert = Command::cargo_bin("shiwake")
        .expect("binary should build")
        .args([
            "--repo",
            repo.to_str().expect("repo path should be utf8"),
            "--base",
            &base,
            "--head",
            &head,
            "--plugin",
            "go",
        ])
        .assert();

    let output = String::from_utf8(assert.get_output().stdout.clone()).expect("stdout is utf8");

    assert!(
        output.contains("\"kind\":\"go_concurrency_change\""),
        "stdout was {output}"
    );
    assert!(output.contains("\"score\":"), "stdout was {output}");
}

#[test]
fn go_plugin_detects_error_handling_change_from_revision_range() {
    let initial = r#"package main

import (
    "context"
)

func Run(ctx context.Context, err error) error {
    if err != nil {
        return err
    }
    return nil
}
"#;
    let updated = r#"package main

import (
    "context"
    "errors"
)

func Run(ctx context.Context, err error) error {
    if err != nil {
        if errors.Is(err, context.DeadlineExceeded) {
            return nil
        }
        return err
    }
    return nil
}
"#;
    let (repo, base, head) = init_go_repo("go-error-handling", initial, updated);

    let assert = Command::cargo_bin("shiwake")
        .expect("binary should build")
        .args([
            "--repo",
            repo.to_str().expect("repo path should be utf8"),
            "--base",
            &base,
            "--head",
            &head,
            "--plugin",
            "go",
        ])
        .assert();

    let output = String::from_utf8(assert.get_output().stdout.clone()).expect("stdout is utf8");

    assert!(
        output.contains("\"kind\":\"go_error_handling_change\""),
        "stdout was {output}"
    );
    assert!(
        !output.contains("\"kind\":\"go_concurrency_change\""),
        "stdout was {output}"
    );
}

#[test]
fn go_plugin_detects_embedded_interface_break_with_type_info() {
    let repo = unique_dir("go-embedded-interface");
    fs::create_dir_all(&repo).expect("repo dir should exist");

    git(&repo, &["init"]);
    git(&repo, &["config", "user.name", "Tomohiro"]);
    git(&repo, &["config", "user.email", "tomohiro@example.com"]);
    git(&repo, &["config", "commit.gpgsign", "false"]);
    write_file(
        &repo.join("go.mod"),
        "module example.com/shiwake-test\n\ngo 1.26.0\n",
    );
    write_file(
        &repo.join("iface/base.go"),
        "package iface\n\nimport \"context\"\n\ntype Base interface {\n    Run(context.Context) error\n}\n",
    );

    let initial = r#"package main

import (
    "context"
    "example.com/shiwake-test/iface"
)

type Runner interface {
    iface.Base
}

type worker struct{}

func (worker) Run(ctx context.Context) error {
    return nil
}
"#;
    let updated = r#"package main

import (
    "context"
    "example.com/shiwake-test/iface"
)

type Runner interface {
    iface.Base
}

type worker struct{}

func (worker) Run(ctx context.Context, strict bool) error {
    return nil
}
"#;
    write_file(&repo.join("main.go"), initial);
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-m", "initial"]);
    let base = git(&repo, &["rev-parse", "HEAD"]);

    write_file(&repo.join("main.go"), updated);
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-m", "update"]);
    let head = git(&repo, &["rev-parse", "HEAD"]);

    let assert = Command::cargo_bin("shiwake")
        .expect("binary should build")
        .args([
            "--repo",
            repo.to_str().expect("repo path should be utf8"),
            "--base",
            &base,
            "--head",
            &head,
            "--plugin",
            "go",
        ])
        .assert();

    let output = String::from_utf8(assert.get_output().stdout.clone()).expect("stdout is utf8");

    assert!(
        output.contains("\"kind\":\"go_interface_break\""),
        "stdout was {output}"
    );
}

#[test]
fn go_plugin_detects_receiver_kind_change() {
    let initial = r#"package main

type Counter struct{}

func (Counter) Inc() int {
    return 1
}
"#;
    let updated = r#"package main

type Counter struct{}

func (*Counter) Inc() int {
    return 1
}
"#;
    let (repo, base, head) = init_go_repo("go-receiver-kind", initial, updated);

    let assert = Command::cargo_bin("shiwake")
        .expect("binary should build")
        .args([
            "--repo",
            repo.to_str().expect("repo path should be utf8"),
            "--base",
            &base,
            "--head",
            &head,
            "--plugin",
            "go",
        ])
        .assert();

    let output = String::from_utf8(assert.get_output().stdout.clone()).expect("stdout is utf8");

    assert!(
        output.contains("\"kind\":\"go_receiver_change\""),
        "stdout was {output}"
    );
}

#[test]
fn go_plugin_detects_test_oracle_change() {
    let repo = unique_dir("go-test-oracle");
    fs::create_dir_all(&repo).expect("repo dir should exist");

    git(&repo, &["init"]);
    git(&repo, &["config", "user.name", "Tomohiro"]);
    git(&repo, &["config", "user.email", "tomohiro@example.com"]);
    git(&repo, &["config", "commit.gpgsign", "false"]);
    write_file(
        &repo.join("go.mod"),
        "module example.com/shiwake-test\n\ngo 1.26.0\n",
    );
    write_file(
        &repo.join("main_test.go"),
        "package main\n\nimport (\n    \"testing\"\n    \"github.com/google/go-cmp/cmp\"\n)\n\nfunc TestValue(t *testing.T) {\n    if diff := cmp.Diff(\"want\", \"got\"); diff != \"\" {\n        t.Fatal(diff)\n    }\n}\n",
    );
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-m", "initial"]);
    let base = git(&repo, &["rev-parse", "HEAD"]);

    write_file(
        &repo.join("main_test.go"),
        "package main\n\nimport (\n    \"testing\"\n    \"github.com/google/go-cmp/cmp\"\n)\n\nfunc TestValue(t *testing.T) {\n    if diff := cmp.Diff(\"want-2\", \"got\"); diff != \"\" {\n        t.Fatal(diff)\n    }\n}\n",
    );
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-m", "update"]);
    let head = git(&repo, &["rev-parse", "HEAD"]);

    let assert = Command::cargo_bin("shiwake")
        .expect("binary should build")
        .args([
            "--repo",
            repo.to_str().expect("repo path should be utf8"),
            "--base",
            &base,
            "--head",
            &head,
            "--plugin",
            "go",
        ])
        .assert();

    let output = String::from_utf8(assert.get_output().stdout.clone()).expect("stdout is utf8");

    assert!(
        output.contains("\"kind\":\"go_test_oracle_change\""),
        "stdout was {output}"
    );
}

#[test]
fn go_plugin_ignores_test_failure_message_only_change() {
    let repo = unique_dir("go-test-message-only");
    fs::create_dir_all(&repo).expect("repo dir should exist");

    git(&repo, &["init"]);
    git(&repo, &["config", "user.name", "Tomohiro"]);
    git(&repo, &["config", "user.email", "tomohiro@example.com"]);
    git(&repo, &["config", "commit.gpgsign", "false"]);
    write_file(
        &repo.join("go.mod"),
        "module example.com/shiwake-test\n\ngo 1.26.0\n",
    );
    write_file(
        &repo.join("main_test.go"),
        "package main\n\nimport \"testing\"\n\nfunc TestValue(t *testing.T) {\n    if got := value(); got != 1 {\n        t.Fatalf(\"unexpected value: %d\", got)\n    }\n}\n",
    );
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-m", "initial"]);
    let base = git(&repo, &["rev-parse", "HEAD"]);

    write_file(
        &repo.join("main_test.go"),
        "package main\n\nimport \"testing\"\n\nfunc TestValue(t *testing.T) {\n    if got := value(); got != 1 {\n        t.Fatalf(\"value mismatch: %d\", got)\n    }\n}\n",
    );
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-m", "update"]);
    let head = git(&repo, &["rev-parse", "HEAD"]);

    let assert = Command::cargo_bin("shiwake")
        .expect("binary should build")
        .args([
            "--repo",
            repo.to_str().expect("repo path should be utf8"),
            "--base",
            &base,
            "--head",
            &head,
            "--plugin",
            "go",
        ])
        .assert();

    let output = String::from_utf8(assert.get_output().stdout.clone()).expect("stdout is utf8");

    assert!(
        !output.contains("\"kind\":\"go_test_oracle_change\""),
        "stdout was {output}"
    );
}

#[test]
fn go_plugin_ignores_assert_message_only_change() {
    let repo = unique_dir("go-assert-message-only");
    fs::create_dir_all(&repo).expect("repo dir should exist");

    git(&repo, &["init"]);
    git(&repo, &["config", "user.name", "Tomohiro"]);
    git(&repo, &["config", "user.email", "tomohiro@example.com"]);
    git(&repo, &["config", "commit.gpgsign", "false"]);
    write_file(
        &repo.join("go.mod"),
        "module example.com/shiwake-test\n\ngo 1.26.0\n",
    );
    write_file(
        &repo.join("main_test.go"),
        "package main\n\nimport (\n    \"testing\"\n    \"github.com/stretchr/testify/require\"\n)\n\nfunc TestValue(t *testing.T) {\n    require.Equalf(t, 1, value(), \"unexpected value\")\n}\n",
    );
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-m", "initial"]);
    let base = git(&repo, &["rev-parse", "HEAD"]);

    write_file(
        &repo.join("main_test.go"),
        "package main\n\nimport (\n    \"testing\"\n    \"github.com/stretchr/testify/require\"\n)\n\nfunc TestValue(t *testing.T) {\n    require.Equalf(t, 1, value(), \"value mismatch\")\n}\n",
    );
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-m", "update"]);
    let head = git(&repo, &["rev-parse", "HEAD"]);

    let assert = Command::cargo_bin("shiwake")
        .expect("binary should build")
        .args([
            "--repo",
            repo.to_str().expect("repo path should be utf8"),
            "--base",
            &base,
            "--head",
            &head,
            "--plugin",
            "go",
        ])
        .assert();

    let output = String::from_utf8(assert.get_output().stdout.clone()).expect("stdout is utf8");

    assert!(
        !output.contains("\"kind\":\"go_test_oracle_change\""),
        "stdout was {output}"
    );
}

#[test]
fn go_plugin_detects_context_retry_time_change() {
    let initial = r#"package main

import (
    "context"
    "time"
)

func Run(ctx context.Context) error {
    _ = time.Second
    _ = ctx
    return nil
}
"#;
    let updated = r#"package main

import (
    "context"
    "time"
)

func Run(ctx context.Context) error {
    for retries := 0; retries < 3; retries++ {
        select {
        case <-ctx.Done():
            return ctx.Err()
        case <-time.After(2 * time.Second):
            continue
        }
    }
    return nil
}
"#;
    let (repo, base, head) = init_go_repo("go-context-retry-time", initial, updated);

    let assert = Command::cargo_bin("shiwake")
        .expect("binary should build")
        .args([
            "--repo",
            repo.to_str().expect("repo path should be utf8"),
            "--base",
            &base,
            "--head",
            &head,
            "--plugin",
            "go",
        ])
        .assert();

    let output = String::from_utf8(assert.get_output().stdout.clone()).expect("stdout is utf8");

    assert!(
        output.contains("\"kind\":\"go_runtime_behavior_change\""),
        "stdout was {output}"
    );
}

#[test]
fn go_plugin_detects_resource_lifecycle_change() {
    let initial = r#"package main

import "context"

func Run(ctx context.Context) error {
    _ = ctx
    return nil
}
"#;
    let updated = r#"package main

import "context"

func Run(ctx context.Context) error {
    ctx, cancel := context.WithCancel(ctx)
    defer cancel()
    _ = ctx
    return nil
}
"#;
    let (repo, base, head) = init_go_repo("go-resource-lifecycle", initial, updated);

    let assert = Command::cargo_bin("shiwake")
        .expect("binary should build")
        .args([
            "--repo",
            repo.to_str().expect("repo path should be utf8"),
            "--base",
            &base,
            "--head",
            &head,
            "--plugin",
            "go",
        ])
        .assert();

    let output = String::from_utf8(assert.get_output().stdout.clone()).expect("stdout is utf8");

    assert!(
        output.contains("\"kind\":\"go_resource_lifecycle_change\""),
        "stdout was {output}"
    );
}
