# shiwake

`shiwake` はコード差分を採点し、機械処理しやすい JSON レポートを返す。

目的は意味的な同値性の証明ではない。差分が人間のレビューを要する程度に重要かを見積もることにある。

現在のヒューリスティクスは次を重く扱う。

- public interface changes
- control-flow changes
- test expectation changes
- language-plugin signals such as Go interface and concurrency changes
- Go error-handling changes such as `errors.Is/As`, `nil` checks, and `context` guards
- Go-specific receiver, runtime-behavior, resource-lifecycle, and test-oracle changes

逆に、次は低く評価する。

- comment-only changes
- import-only changes
- refactor-like renames

`.gitattributes` で `linguist-generated=true` が付いたファイルは採点対象から除外される。後続の `linguist-generated=false` override も尊重する。

## Quick Start

ローカル開発中は Cargo から直接実行する。

```bash
cargo run -- --repo . --patch sample.patch
```

CLI は 1 行 JSON を出力する。見やすくしたい場合は `jq` を通す。

```bash
cargo run -- --repo . --patch sample.patch | jq
```

グローバルに入れる場合:

```bash
make install
```

削除する場合:

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

staged diff もそのまま流せる。

```bash
git diff --cached | cargo run -- --repo . --patch -
```

`--repo` が `.gitattributes` を持つリポジトリを指している場合、patch mode でも generated file は除外される。

### Git Revisions

```bash
cargo run -- --repo . --base HEAD~1 --head HEAD
```

この mode では `git2` でリポジトリを開き、2 revision 間の patch を生成し、file history metadata を付けてから同じ scorer を走らせる。

## Plugins

有効化は `--plugin` で行う。

```bash
cargo run -- --repo . --base HEAD~1 --head HEAD --plugin go
cargo run -- --repo . --base HEAD~1 --head HEAD --plugin ts
```

現在の built-in plugin ID:

- `go`
- `ts`

Go plugin は repository revisions を使って、次の高精度チェックを追加できる。

- exported API and interface break detection
- receiver kind changes such as value-to-pointer receiver flips
- concurrency and runtime behavior changes
- error-handling and resource-lifecycle changes
- Go test-oracle changes such as `cmp.Diff`, `assert`, `require`, and `t.Fatal`

TypeScript plugin は repository revisions を使って、次の高精度チェックを追加できる。

- exported API and interface break detection, including relative-imported interfaces and simple type aliases
- member kind changes such as method-to-property flips
- async and runtime behavior changes such as `async`/`await`, `Promise`, timers, and retry markers
- error-handling and resource-lifecycle changes
- TypeScript test-oracle changes such as `expect(...).toBe(...)`

## Score Configuration

デフォルトでは built-in の `v1` scoring model を使う。

weights、thresholds、aggregation を上書きしたい場合は `--config` で TOML を渡す。

```bash
cargo run -- --repo . --patch sample.patch --config custom-score.toml
```

例:

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

`kind` の値自体はコード側で固定されている。設定で変えられるのは採点だけで、pattern 定義ではない。`gitattributes_skip_attributes` には、除外トリガーとして扱う `.gitattributes` attribute 名を並べる。

## Scoring Model

scorer は単純な線形和ではない。3 段階で計算する。

1. file ごとに最も強い base reason を選ぶ
2. `change_size` や `repo_hotspot` のような bounded modifier を足す
3. patch 全体を `top file score + bounded secondary contribution` として集計する

これで semantic risk を主因に保ちつつ、patch size や repo history で調整できる。

## Output

### Example

```json
{"schema_version":"1","scoring_model_version":"v1","score":79,"decision":"review_required","confidence":"high","secondary_contribution":0,"reasons":[{"kind":"public_interface_change","file":"src/lib.rs","weight":75,"message":"public interface changed"},{"kind":"change_size","file":"src/lib.rs","weight":4,"message":"change size increased review load (4 changed lines)"}],"by_file":[{"path":"src/lib.rs","score":79,"language":"rust","base_score":75,"size_modifier":4,"hotspot_modifier":0,"plugin_contribution":0}],"feature_vector":{"files_changed":1,"public_signature_changes":1,"control_flow_changes":0,"assertion_changes":0,"size_signals":1,"hotspot_signals":0,"plugin_signals":0}}
```

### Top-Level Fields

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

各 `by_file` entry は次を含む。

- `base_score`: その file の strongest core signal
- `size_modifier`: その file に対する bounded size bump
- `hotspot_modifier`: その file に対する bounded history bump
- `plugin_contribution`: plugin が additive-only signal を出したときの追加 weight

`score` はこれらを合成した最終的な per-file total である。

## Library Usage

crate は library としても使える。

```rust
use shiwake::analyze_patch;

let patch = std::fs::read_to_string("sample.patch")?;
let report = analyze_patch(&patch, &[])?;
println!("{}", report.score);
# Ok::<(), Box<dyn std::error::Error>>(())
```

custom scoring model を読む場合:

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
