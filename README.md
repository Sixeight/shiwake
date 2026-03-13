# shiwake

`shiwake` scores a code diff and returns a machine-readable JSON report.

The goal is not to prove semantic equivalence. The goal is to estimate whether a diff is important enough to deserve human review.

Current heuristics favor:

- public interface changes
- control-flow changes
- test expectation changes
- language-plugin signals such as Go interface and concurrency changes
- Go error-handling changes such as `errors.Is/As`, `nil` checks, and `context` guards
- Go-specific receiver, runtime-behavior, resource-lifecycle, and test-oracle changes

Current heuristics down-rank:

- comment-only changes
- import-only changes
- refactor-like renames

## Install And Run

Use Cargo directly while the project is still local-only.

```bash
cargo run -- --repo . --patch sample.patch
```

The CLI prints compact one-line JSON so the caller can format it however it wants.

```bash
cargo run -- --repo . --patch sample.patch | jq
```

To install the binary globally through Cargo:

```bash
make install
```

To remove it again:

```bash
make uninstall
```

## Input Modes

### Read A Patch File

```bash
cargo run -- --repo . --patch sample.patch
```

### Read From Standard Input

```bash
git diff | cargo run -- --repo . --patch -
```

You can also score staged changes.

```bash
git diff --cached | cargo run -- --repo . --patch -
```

### Compare Two Git Revisions

```bash
cargo run -- --repo . --base HEAD~1 --head HEAD
```

This mode opens the repository with `git2`, generates a patch between two revisions, attaches file history metadata, then runs the same scorer.

### Enable Built-In Plugins

```bash
cargo run -- --repo . --base HEAD~1 --head HEAD --plugin go
```

Current built-in plugin IDs:

- `go`

The Go plugin can use repository revisions for higher-precision checks such as:

- exported API and interface break detection
- receiver kind changes such as value-to-pointer receiver flips
- concurrency and runtime behavior changes
- error-handling and resource-lifecycle changes
- Go test-oracle changes such as `cmp.Diff`, `assert`, `require`, and `t.Fatal`

## Score Configuration

By default, the built-in `v1` scoring model is used.

To override weights, thresholds, or aggregation behavior, pass a TOML file with `--config`.

```bash
cargo run -- --repo . --patch sample.patch --config custom-score.toml
```

Example:

```toml
schema_version = 1
scoring_model_version = "custom-v1"

[decision_thresholds]
skip_review_max = 24
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

`kind` values are fixed by the code. The configuration controls scoring, not pattern definitions.

## Scoring Model

The scorer is no longer a simple linear sum of every signal.

It works in three stages:

1. pick the strongest per-file base reason
2. add bounded per-file modifiers such as `change_size` and `repo_hotspot`
3. aggregate the patch as `top file score + bounded secondary contribution`

This keeps semantic risk as the main driver while letting patch size and repo history adjust the result without overwhelming it.

## Example Output

```json
{"schema_version":"1","scoring_model_version":"v1","score":79,"decision":"review_required","confidence":"high","secondary_contribution":0,"reasons":[{"kind":"public_interface_change","file":"src/lib.rs","weight":75,"message":"public interface changed"},{"kind":"change_size","file":"src/lib.rs","weight":4,"message":"change size increased review load (4 changed lines)"}],"by_file":[{"path":"src/lib.rs","score":79,"language":"rust","base_score":75,"size_modifier":4,"hotspot_modifier":0,"plugin_contribution":0}],"feature_vector":{"files_changed":1,"public_signature_changes":1,"control_flow_changes":0,"assertion_changes":0,"size_signals":1,"hotspot_signals":0,"plugin_signals":0}}
```

## How To Read The Result

### Top-Level Fields

- `score`: raw score from `0` to `100`
- `decision`: default review recommendation derived from the score
- `confidence`: confidence in the analysis result
- `secondary_contribution`: bounded contribution added by non-top files
- `reasons`: the rule hits that explain the score
- `by_file`: per-file score breakdown
- `feature_vector`: coarse counters used during aggregation

### Default Decision Thresholds

- `0-24`: `skip_review`
- `25-59`: `review_recommended`
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
- `go_error_handling_change`: Go error, context, panic/recover, or nil-guard handling changed
- `go_receiver_change`: Go method receiver kind changed
- `go_test_oracle_change`: Go test oracle or expectation logic changed
- `go_runtime_behavior_change`: Go time, retry, or context-driven runtime behavior changed
- `go_resource_lifecycle_change`: Go cleanup and resource lifecycle handling changed
- `go_analysis_fallback`: Go plugin could not run high-precision analysis and fell back

### Per-File Breakdown

Each `by_file` entry contains:

- `base_score`: strongest core signal on that file
- `size_modifier`: bounded size bump for that file
- `hotspot_modifier`: bounded history bump for that file
- `plugin_contribution`: extra plugin-added weight when a plugin emits additive-only signals

`score` is the final per-file total after combining these pieces.

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
# Ok::<(), Box<dyn std::error::Error>>(())
```

## Current Scope

- The core scorer is intentionally AST-free.
- AST-aware analysis is expected to come from plugins.
- The current model is heuristic and explainable, not learned from historical review outcomes.
- Git revision analysis can use repo history; plain patch input cannot.
