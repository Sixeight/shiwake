# shiwake

`shiwake` scores code diffs and emits machine-readable JSON reports.

Its goal is not to prove semantic equivalence. It estimates whether a diff is important enough to deserve human review.

This project is licensed under MIT. See [LICENSE](./LICENSE) for details.

The current heuristics weigh the following changes heavily:

- public interface changes
- control-flow changes
- test expectation changes
- language-plugin signals such as Go interface and concurrency changes
- Go error-handling changes such as `errors.Is/As`, `nil` checks, and `context` guards
- Go-specific receiver, runtime-behavior, resource-lifecycle, and test-oracle changes

By contrast, the following changes are scored lower:

- comment-only changes
- import-only changes
- refactor-like renames

Files marked with `linguist-generated=true` in `.gitattributes` are excluded from scoring. Later `linguist-generated=false` overrides are also respected.

## Requirements

The minimum requirements are:

- Rust toolchain
- `git`

Plugins require additional tools:

- `--plugin go`: `go`
- `--plugin ts`: `node`

Revision range analysis and high-precision plugin analysis assume the target repository exists locally and that `--repo` points at its root.

## Quick Start

During local development, run it directly with Cargo.

```bash
cargo run -- --repo . --patch sample.patch
```

The CLI prints a single-line JSON report. Pipe it through `jq` if you want formatted output.

```bash
cargo run -- --repo . --patch sample.patch | jq
```

To install it globally:

```bash
make install
```

To uninstall it:

```bash
make uninstall
```

## Input Modes

### Patch File

```bash
cargo run -- --repo . --patch sample.patch
```

### Standard Input

```bash
git diff | cargo run -- --repo . --patch -
```

You can also pipe a staged diff directly.

```bash
git diff --cached | cargo run -- --repo . --patch -
```

If `--repo` points to a repository with `.gitattributes`, generated files are excluded even in patch mode.

### Git Revisions

```bash
cargo run -- --repo . --base HEAD~1 --head HEAD
```

In this mode, `git2` opens the repository, generates a patch between the two revisions, attaches file-history metadata, and then runs the same scorer.

## Plugins

Enable plugins with `--plugin`.

```bash
cargo run -- --repo . --base HEAD~1 --head HEAD --plugin go
cargo run -- --repo . --base HEAD~1 --head HEAD --plugin ts
```

Current built-in plugin IDs:

- `go`
- `ts`

Constraints:

- High-precision plugin analysis is only available with revision range input using `--base` and `--head`
- The `go` plugin requires `go.mod` to exist in both revisions
- The `ts` plugin requires `package.json` to exist in both revisions
- If a prerequisite is missing, the plugin returns fallback signals instead of failing

The Go plugin uses repository revisions to add higher-precision checks for:

- exported API and interface break detection
- receiver kind changes such as value-to-pointer receiver flips
- concurrency and runtime behavior changes
- error-handling and resource-lifecycle changes
- Go test-oracle changes such as `cmp.Diff`, `assert`, `require`, and `t.Fatal`

The TypeScript plugin uses repository revisions to add higher-precision checks for:

- exported API and interface break detection, including relative-imported interfaces and simple type aliases
- member kind changes such as method-to-property flips
- async and runtime behavior changes such as `async`/`await`, `Promise`, timers, and retry markers
- error-handling and resource-lifecycle changes
- TypeScript test-oracle changes such as `expect(...).toBe(...)`

## Score Configuration

By default, the built-in `v1` scoring model is used.

Pass a TOML file with `--config` if you want to override weights, thresholds, or aggregation.

```bash
cargo run -- --repo . --patch sample.patch --config custom-score.toml
```

Example:

```toml
schema_version = 1
scoring_model_version = "custom-v1"
gitattributes_skip_attributes = ["linguist-generated", "pr-review-skip"]

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
kind = "comment_only"
score = 0

[[rules]]
kind = "public_interface_change"
score = 75

[[rules]]
kind = "control_flow_change"
score = 65

[[rules]]
kind = "test_expectation_change"
score = 55

[[rules]]
kind = "generic_code_change"
score = 20
```

The set of `kind` values is fixed in code. Configuration can change scoring only, not pattern definitions. `gitattributes_skip_attributes` lists `.gitattributes` attribute names that should act as exclusion triggers.

## Scoring Model

The scorer is not a simple linear sum. It runs in three stages:

1. Pick the strongest base reason for each file
2. Add bounded modifiers such as `change_size` and `repo_hotspot`
3. Aggregate the whole patch as `top file score + bounded secondary contribution`

This keeps semantic risk as the primary driver while still adjusting for patch size and repository history.

## Output

### Example

```json
{"schema_version":"1","scoring_model_version":"v1","score":79,"decision":"review_required","confidence":"high","secondary_contribution":0,"reasons":[{"kind":"public_interface_change","file":"src/lib.rs","weight":75,"message":"public interface changed"},{"kind":"change_size","file":"src/lib.rs","weight":4,"message":"change size increased review load (4 changed lines)"}],"by_file":[{"path":"src/lib.rs","score":79,"language":"rust","base_score":75,"size_modifier":4,"hotspot_modifier":0,"plugin_contribution":0}],"feature_vector":{"files_changed":1,"public_signature_changes":1,"control_flow_changes":0,"assertion_changes":0,"size_signals":1,"hotspot_signals":0,"plugin_signals":0}}
```

### Top-Level Fields

- `schema_version`: report schema version
- `scoring_model_version`: scoring model version
- `score`: raw score from `0` to `100`
- `decision`: default review recommendation derived from the score
- `confidence`: confidence in the analysis result
- `secondary_contribution`: bounded contribution added by non-top files
- `reasons`: rule hits that explain the score
- `by_file`: per-file score breakdown
- `feature_vector`: coarse counters used during aggregation

### Default Decision Thresholds

- `0-24`: `skip_review`
- `25-29`: `review_optional`
- `30-39`: `review_suggested`
- `40-59`: `review_recommended`
- `60+`: `review_required`

### Common Reason Kinds

- `comment_only`: comments or whitespace only
- `import_only`: import or package declaration changes only
- `refactor_like_change`: rename-like or structure-preserving change
- `change_size`: larger patches get an additive review-load bump
- `repo_hotspot`: frequently touched files get an additive hotspot bump
- `public_interface_change`: exported API or signature changed
- `control_flow_change`: branching or flow control changed
- `test_expectation_change`: assertions or expected values changed
- `generic_code_change`: fallback for code changes that do not match a stronger rule
- `go_exported_api_change`: Go exported API changed
- `go_interface_break`: Go type no longer satisfies an interface contract
- `go_concurrency_change`: Go concurrency primitive or behavior changed
- `go_error_handling_change`: Go error, context, panic/recover, or nil-guard handling changed
- `go_receiver_change`: Go method receiver kind changed
- `go_test_oracle_change`: Go test oracle or expectation logic changed
- `go_runtime_behavior_change`: Go time, retry, or context-driven runtime behavior changed
- `go_resource_lifecycle_change`: Go cleanup and resource lifecycle handling changed
- `go_analysis_fallback`: Go plugin could not run high-precision analysis and fell back
- `typescript_exported_api_change`: TypeScript exported API changed
- `typescript_interface_break`: TypeScript class no longer satisfies an interface or object-like type alias
- `typescript_async_change`: TypeScript async, await, promise, or timer behavior changed
- `typescript_error_handling_change`: TypeScript try/catch/throw or error narrowing changed
- `typescript_member_kind_change`: TypeScript class member kind changed
- `typescript_test_oracle_change`: TypeScript test oracle or expectation logic changed
- `typescript_runtime_behavior_change`: TypeScript time, retry, or runtime behavior changed
- `typescript_resource_lifecycle_change`: TypeScript cleanup or abort lifecycle handling changed
- `typescript_analysis_fallback`: TypeScript plugin could not run high-precision analysis and fell back

### Per-File Breakdown

Each `by_file` entry contains:

- `path`: file path
- `score`: final per-file score
- `language`: detected language
- `base_score`: the strongest core signal for that file
- `size_modifier`: the bounded size bump applied to that file
- `hotspot_modifier`: the bounded history bump applied to that file
- `plugin_contribution`: extra weight added when a plugin emits additive-only signals

`score` is the final per-file total after combining those values.

## Library Usage

The crate can also be used as a library.

```rust
use shiwake::analyze_patch;

let patch = std::fs::read_to_string("sample.patch")?;
let report = analyze_patch(&patch, &[])?;
println!("{}", report.score);
# Ok::<(), Box<dyn std::error::Error>>(())
```

To load a custom scoring model:

```rust
use shiwake::{ScoreConfig, analyze_patch_with_config};

let patch = std::fs::read_to_string("sample.patch")?;
let config_text = std::fs::read_to_string("custom-score.toml")?;
let config = ScoreConfig::from_toml(&config_text)?;
let report = analyze_patch_with_config(&patch, &[], &config)?;
println!("{}", report.decision.as_str());
```

## Current Scope

- The core scorer is intentionally AST-free.
- AST-aware analysis is expected to come from plugins.
- The current model is heuristic and explainable, not learned from historical review outcomes.
- Git revision analysis can use repo history; plain patch input cannot.
