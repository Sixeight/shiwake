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

fn init_typescript_repo(
    name: &str,
    initial_files: &[(&str, &str)],
    updated_files: &[(&str, &str)],
) -> (PathBuf, String, String) {
    let repo = unique_dir(name);
    fs::create_dir_all(&repo).expect("repo dir should exist");

    git(&repo, &["init"]);
    git(&repo, &["config", "user.name", "Tomohiro"]);
    git(&repo, &["config", "user.email", "tomohiro@example.com"]);
    git(&repo, &["config", "commit.gpgsign", "false"]);

    write_file(
        &repo.join("package.json"),
        "{\n  \"name\": \"shiwake-ts-test\",\n  \"private\": true,\n  \"type\": \"module\"\n}\n",
    );

    for (path, contents) in initial_files {
        write_file(&repo.join(path), contents);
    }
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-m", "initial"]);
    let base = git(&repo, &["rev-parse", "HEAD"]);

    for (path, contents) in updated_files {
        write_file(&repo.join(path), contents);
    }
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-m", "update"]);
    let head = git(&repo, &["rev-parse", "HEAD"]);

    (repo, base, head)
}

#[test]
fn typescript_plugin_detects_exported_api_change_from_revision_range() {
    let initial = r#"export interface Runner {
  run(id: string): Promise<void>
}

export class Worker implements Runner {
  async run(id: string): Promise<void> {
    void id
  }
}

export function build(id: string): Promise<void> {
  return new Worker().run(id)
}
"#;
    let updated = r#"export interface Runner {
  run(id: string): Promise<void>
}

export class Worker implements Runner {
  async run(id: string, strict: boolean): Promise<void> {
    void id
    void strict
  }
}

export function build(id: string, strict: boolean): Promise<void> {
  return new Worker().run(id, strict)
}
"#;
    let (repo, base, head) = init_typescript_repo(
        "ts-api",
        &[("src/api.ts", initial)],
        &[("src/api.ts", updated)],
    );

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
            "ts",
        ])
        .assert();

    let output = String::from_utf8(assert.get_output().stdout.clone()).expect("stdout is utf8");

    assert!(
        output.contains("\"kind\":\"typescript_exported_api_change\""),
        "stdout was {output}"
    );
    assert!(
        output.contains("\"kind\":\"typescript_interface_break\""),
        "stdout was {output}"
    );
}

#[test]
fn typescript_plugin_detects_async_change_from_revision_range() {
    let initial = r#"export function run(): number {
  return 1
}
"#;
    let updated = r#"export async function run(signal?: AbortSignal): Promise<number> {
  return await Promise.resolve(signal ? 2 : 1)
}
"#;
    let (repo, base, head) = init_typescript_repo(
        "ts-async",
        &[("src/run.ts", initial)],
        &[("src/run.ts", updated)],
    );

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
            "ts",
        ])
        .assert();

    let output = String::from_utf8(assert.get_output().stdout.clone()).expect("stdout is utf8");

    assert!(
        output.contains("\"kind\":\"typescript_async_change\""),
        "stdout was {output}"
    );
}

#[test]
fn typescript_plugin_falls_back_for_patch_input() {
    let initial = r#"export function build(id: string): string {
  return id
}
"#;
    let updated = r#"export function build(id: string, strict: boolean): string {
  return strict ? id.trim() : id
}
"#;
    let (repo, base, head) = init_typescript_repo(
        "ts-fallback",
        &[("src/build.ts", initial)],
        &[("src/build.ts", updated)],
    );
    let patch = git(&repo, &["diff", &base, &head]);
    let patch_path = unique_dir("ts-fallback-patch");
    write_file(&patch_path, &patch);

    let assert = Command::cargo_bin("shiwake")
        .expect("binary should build")
        .args([
            "--repo",
            repo.to_str().expect("repo path should be utf8"),
            "--patch",
            patch_path.to_str().expect("patch path should be utf8"),
            "--plugin",
            "ts",
        ])
        .assert();

    let output = String::from_utf8(assert.get_output().stdout.clone()).expect("stdout is utf8");

    assert!(
        output.contains("\"kind\":\"typescript_analysis_fallback\""),
        "stdout was {output}"
    );
    assert!(
        output.contains("\"confidence\":\"medium\""),
        "stdout was {output}"
    );
}

#[test]
fn typescript_plugin_detects_error_handling_change() {
    let initial = r#"export function run(err?: Error): string {
  if (err) {
    return err.message
  }
  return "ok"
}
"#;
    let updated = r#"export function run(err?: Error): string {
  try {
    if (err) {
      throw err
    }
    return "ok"
  } catch (error) {
    if (error instanceof Error) {
      return error.message
    }
    return "unknown"
  }
}
"#;
    let (repo, base, head) = init_typescript_repo(
        "ts-error",
        &[("src/error.ts", initial)],
        &[("src/error.ts", updated)],
    );

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
            "ts",
        ])
        .assert();

    let output = String::from_utf8(assert.get_output().stdout.clone()).expect("stdout is utf8");

    assert!(
        output.contains("\"kind\":\"typescript_error_handling_change\""),
        "stdout was {output}"
    );
}

#[test]
fn typescript_plugin_detects_imported_interface_break() {
    let initial_base = r#"export interface BaseRunner {
  run(id: string): Promise<void>
}
"#;
    let updated_base = initial_base;
    let initial_main = r#"import type { BaseRunner } from "../iface/base"

export interface Runner extends BaseRunner {}

export class Worker implements Runner {
  async run(id: string): Promise<void> {
    void id
  }
}
"#;
    let updated_main = r#"import type { BaseRunner } from "../iface/base"

export interface Runner extends BaseRunner {}

export class Worker implements Runner {
  async run(id: string, strict: boolean): Promise<void> {
    void id
    void strict
  }
}
"#;
    let (repo, base, head) = init_typescript_repo(
        "ts-imported-interface",
        &[
            ("src/iface/base.ts", initial_base),
            ("src/main/api.ts", initial_main),
        ],
        &[
            ("src/iface/base.ts", updated_base),
            ("src/main/api.ts", updated_main),
        ],
    );

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
            "ts",
        ])
        .assert();

    let output = String::from_utf8(assert.get_output().stdout.clone()).expect("stdout is utf8");

    assert!(
        output.contains("\"kind\":\"typescript_interface_break\""),
        "stdout was {output}"
    );
}

#[test]
fn typescript_plugin_detects_type_alias_interface_break() {
    let initial = r#"export type Runner = {
  run(id: string): Promise<void>
}

export class Worker implements Runner {
  async run(id: string): Promise<void> {
    void id
  }
}
"#;
    let updated = r#"export type Runner = {
  run(id: string): Promise<void>
}

export class Worker implements Runner {
  async run(id: string, strict: boolean): Promise<void> {
    void id
    void strict
  }
}
"#;
    let (repo, base, head) = init_typescript_repo(
        "ts-type-alias-interface",
        &[("src/runner.ts", initial)],
        &[("src/runner.ts", updated)],
    );

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
            "ts",
        ])
        .assert();

    let output = String::from_utf8(assert.get_output().stdout.clone()).expect("stdout is utf8");

    assert!(
        output.contains("\"kind\":\"typescript_interface_break\""),
        "stdout was {output}"
    );
}

#[test]
fn typescript_plugin_detects_member_kind_change() {
    let initial = r#"export class Counter {
  count(): number {
    return 1
  }
}
"#;
    let updated = r#"export class Counter {
  count = (): number => {
    return 1
  }
}
"#;
    let (repo, base, head) = init_typescript_repo(
        "ts-member-kind",
        &[("src/counter.ts", initial)],
        &[("src/counter.ts", updated)],
    );

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
            "ts",
        ])
        .assert();

    let output = String::from_utf8(assert.get_output().stdout.clone()).expect("stdout is utf8");

    assert!(
        output.contains("\"kind\":\"typescript_member_kind_change\""),
        "stdout was {output}"
    );
}

#[test]
fn typescript_plugin_detects_test_oracle_change() {
    let initial = r#"import { expect, test } from "vitest"

test("value", () => {
  expect(value()).toBe("old")
})
"#;
    let updated = r#"import { expect, test } from "vitest"

test("value", () => {
  expect(value()).toBe("new")
})
"#;
    let (repo, base, head) = init_typescript_repo(
        "ts-test-oracle",
        &[("src/value.test.ts", initial)],
        &[("src/value.test.ts", updated)],
    );

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
            "ts",
        ])
        .assert();

    let output = String::from_utf8(assert.get_output().stdout.clone()).expect("stdout is utf8");

    assert!(
        output.contains("\"kind\":\"typescript_test_oracle_change\""),
        "stdout was {output}"
    );
}

#[test]
fn typescript_plugin_detects_runtime_behavior_change() {
    let initial = r#"export function run(): number {
  return 1
}
"#;
    let updated = r#"export async function run(): Promise<number> {
  for (let retries = 0; retries < 3; retries += 1) {
    await new Promise((resolve) => setTimeout(resolve, 100))
  }
  return Date.now()
}
"#;
    let (repo, base, head) = init_typescript_repo(
        "ts-runtime",
        &[("src/runtime.ts", initial)],
        &[("src/runtime.ts", updated)],
    );

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
            "ts",
        ])
        .assert();

    let output = String::from_utf8(assert.get_output().stdout.clone()).expect("stdout is utf8");

    assert!(
        output.contains("\"kind\":\"typescript_runtime_behavior_change\""),
        "stdout was {output}"
    );
}

#[test]
fn typescript_plugin_detects_resource_lifecycle_change() {
    let initial = r#"export async function run(): Promise<void> {
  return
}
"#;
    let updated = r#"export async function run(): Promise<void> {
  const controller = new AbortController()
  const timeout = setTimeout(() => controller.abort(), 100)
  clearTimeout(timeout)
}
"#;
    let (repo, base, head) = init_typescript_repo(
        "ts-resource",
        &[("src/resource.ts", initial)],
        &[("src/resource.ts", updated)],
    );

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
            "ts",
        ])
        .assert();

    let output = String::from_utf8(assert.get_output().stdout.clone()).expect("stdout is utf8");

    assert!(
        output.contains("\"kind\":\"typescript_resource_lifecycle_change\""),
        "stdout was {output}"
    );
}
