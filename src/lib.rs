use std::{
    collections::HashMap,
    fmt, fs,
    path::{Path, PathBuf},
    process::Command,
};

use git2::{Repository, Tree};
use globset::{Glob, GlobMatcher};
use serde::{Deserialize, Serialize};

const SCORING_MODEL_VERSION: &str = "v1";

pub mod plugins;

#[derive(Debug)]
pub enum AnalyzeError {
    EmptyPatch,
    InvalidPatch,
    InvalidConfig(toml::de::Error),
    Io(std::io::Error),
    Git(git2::Error),
    Utf8(std::string::FromUtf8Error),
    Command(String),
}

impl fmt::Display for AnalyzeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyPatch => f.write_str("patch is empty"),
            Self::InvalidPatch => f.write_str("patch does not contain any changed files"),
            Self::InvalidConfig(error) => write!(f, "invalid config: {error}"),
            Self::Io(error) => write!(f, "io error: {error}"),
            Self::Git(error) => write!(f, "git error: {error}"),
            Self::Utf8(error) => write!(f, "utf8 error: {error}"),
            Self::Command(error) => write!(f, "command failed: {error}"),
        }
    }
}

impl std::error::Error for AnalyzeError {}

impl From<std::io::Error> for AnalyzeError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<git2::Error> for AnalyzeError {
    fn from(value: git2::Error) -> Self {
        Self::Git(value)
    }
}

impl From<std::string::FromUtf8Error> for AnalyzeError {
    fn from(value: std::string::FromUtf8Error) -> Self {
        Self::Utf8(value)
    }
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    SkipReview,
    ReviewOptional,
    ReviewSuggested,
    ReviewRecommended,
    ReviewRequired,
}

impl Decision {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::SkipReview => "skip_review",
            Self::ReviewOptional => "review_optional",
            Self::ReviewSuggested => "review_suggested",
            Self::ReviewRecommended => "review_recommended",
            Self::ReviewRequired => "review_required",
        }
    }
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Confidence {
    Low,
    Medium,
    High,
}

impl Confidence {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
        }
    }
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReasonKind {
    CommentOnly,
    ImportOnly,
    ChangeSize,
    RepoHotspot,
    RefactorLikeChange,
    PublicInterfaceChange,
    ControlFlowChange,
    TestExpectationChange,
    GenericCodeChange,
    PluginSignal,
    GoExportedApiChange,
    GoInterfaceBreak,
    GoConcurrencyChange,
    GoErrorHandlingChange,
    GoReceiverChange,
    GoTestOracleChange,
    GoRuntimeBehaviorChange,
    GoResourceLifecycleChange,
    GoAnalysisFallback,
    #[serde(rename = "typescript_exported_api_change")]
    TypeScriptExportedApiChange,
    #[serde(rename = "typescript_interface_break")]
    TypeScriptInterfaceBreak,
    #[serde(rename = "typescript_async_change")]
    TypeScriptAsyncChange,
    #[serde(rename = "typescript_error_handling_change")]
    TypeScriptErrorHandlingChange,
    #[serde(rename = "typescript_member_kind_change")]
    TypeScriptMemberKindChange,
    #[serde(rename = "typescript_test_oracle_change")]
    TypeScriptTestOracleChange,
    #[serde(rename = "typescript_runtime_behavior_change")]
    TypeScriptRuntimeBehaviorChange,
    #[serde(rename = "typescript_resource_lifecycle_change")]
    TypeScriptResourceLifecycleChange,
    #[serde(rename = "typescript_analysis_fallback")]
    TypeScriptAnalysisFallback,
}

impl ReasonKind {
    pub fn as_reason(&self, file: String, weight: u32, message: impl Into<String>) -> Reason {
        Reason {
            kind: self.clone(),
            file,
            weight,
            message: message.into(),
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct RuleConfig {
    pub kind: ReasonKind,
    pub score: u32,
}

#[derive(Clone, Debug, Deserialize)]
pub struct DecisionThresholds {
    pub skip_review_max: u32,
    pub review_optional_max: u32,
    pub review_suggested_max: u32,
    pub review_recommended_max: u32,
}

#[derive(Clone, Debug, Deserialize)]
pub struct AggregationConfig {
    pub max_score: u32,
    pub secondary_ratio: f64,
    pub secondary_cap: u32,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ScoreConfig {
    pub schema_version: u32,
    pub scoring_model_version: String,
    pub decision_thresholds: DecisionThresholds,
    pub aggregation: AggregationConfig,
    #[serde(default = "default_gitattributes_skip_attributes")]
    pub gitattributes_skip_attributes: Vec<String>,
    pub rules: Vec<RuleConfig>,
}

impl ScoreConfig {
    pub fn default_v1() -> Self {
        Self {
            schema_version: 1,
            scoring_model_version: SCORING_MODEL_VERSION.to_string(),
            decision_thresholds: DecisionThresholds {
                skip_review_max: 24,
                review_optional_max: 29,
                review_suggested_max: 39,
                review_recommended_max: 59,
            },
            aggregation: AggregationConfig {
                max_score: 100,
                secondary_ratio: 0.2,
                secondary_cap: 12,
            },
            gitattributes_skip_attributes: default_gitattributes_skip_attributes(),
            rules: vec![
                RuleConfig {
                    kind: ReasonKind::CommentOnly,
                    score: 0,
                },
                RuleConfig {
                    kind: ReasonKind::ImportOnly,
                    score: 5,
                },
                RuleConfig {
                    kind: ReasonKind::ChangeSize,
                    score: 25,
                },
                RuleConfig {
                    kind: ReasonKind::RepoHotspot,
                    score: 15,
                },
                RuleConfig {
                    kind: ReasonKind::RefactorLikeChange,
                    score: 10,
                },
                RuleConfig {
                    kind: ReasonKind::PublicInterfaceChange,
                    score: 75,
                },
                RuleConfig {
                    kind: ReasonKind::ControlFlowChange,
                    score: 65,
                },
                RuleConfig {
                    kind: ReasonKind::TestExpectationChange,
                    score: 55,
                },
                RuleConfig {
                    kind: ReasonKind::GenericCodeChange,
                    score: 20,
                },
                RuleConfig {
                    kind: ReasonKind::PluginSignal,
                    score: 10,
                },
                RuleConfig {
                    kind: ReasonKind::GoExportedApiChange,
                    score: 25,
                },
                RuleConfig {
                    kind: ReasonKind::GoInterfaceBreak,
                    score: 30,
                },
                RuleConfig {
                    kind: ReasonKind::GoConcurrencyChange,
                    score: 20,
                },
                RuleConfig {
                    kind: ReasonKind::GoErrorHandlingChange,
                    score: 25,
                },
                RuleConfig {
                    kind: ReasonKind::GoReceiverChange,
                    score: 25,
                },
                RuleConfig {
                    kind: ReasonKind::GoTestOracleChange,
                    score: 30,
                },
                RuleConfig {
                    kind: ReasonKind::GoRuntimeBehaviorChange,
                    score: 25,
                },
                RuleConfig {
                    kind: ReasonKind::GoResourceLifecycleChange,
                    score: 25,
                },
                RuleConfig {
                    kind: ReasonKind::GoAnalysisFallback,
                    score: 0,
                },
                RuleConfig {
                    kind: ReasonKind::TypeScriptExportedApiChange,
                    score: 25,
                },
                RuleConfig {
                    kind: ReasonKind::TypeScriptInterfaceBreak,
                    score: 30,
                },
                RuleConfig {
                    kind: ReasonKind::TypeScriptAsyncChange,
                    score: 20,
                },
                RuleConfig {
                    kind: ReasonKind::TypeScriptErrorHandlingChange,
                    score: 25,
                },
                RuleConfig {
                    kind: ReasonKind::TypeScriptMemberKindChange,
                    score: 25,
                },
                RuleConfig {
                    kind: ReasonKind::TypeScriptTestOracleChange,
                    score: 30,
                },
                RuleConfig {
                    kind: ReasonKind::TypeScriptRuntimeBehaviorChange,
                    score: 25,
                },
                RuleConfig {
                    kind: ReasonKind::TypeScriptResourceLifecycleChange,
                    score: 25,
                },
                RuleConfig {
                    kind: ReasonKind::TypeScriptAnalysisFallback,
                    score: 0,
                },
            ],
        }
    }

    pub fn from_toml(input: &str) -> Result<Self, AnalyzeError> {
        toml::from_str(input).map_err(AnalyzeError::InvalidConfig)
    }

    pub fn score_for(&self, kind: &ReasonKind) -> u32 {
        self.rules
            .iter()
            .find(|rule| &rule.kind == kind)
            .map(|rule| rule.score)
            .unwrap_or_default()
    }
}

fn default_gitattributes_skip_attributes() -> Vec<String> {
    vec![String::from("linguist-generated")]
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct Reason {
    pub kind: ReasonKind,
    pub file: String,
    pub weight: u32,
    pub message: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InputKind {
    PatchText,
    GitRevisionRange,
}

#[derive(Clone, Debug)]
pub enum AnalyzeInput {
    PatchText {
        patch: String,
    },
    GitRevisionRange {
        repo_root: PathBuf,
        base: String,
        head: String,
    },
}

#[derive(Clone, Debug)]
pub struct AnalyzeRequest {
    pub input: AnalyzeInput,
    pub repo_root: Option<PathBuf>,
}

#[derive(Clone, Debug)]
pub struct ChangedFile {
    pub path: String,
    pub old_path: Option<String>,
    pub new_path: Option<String>,
    pub added: Vec<String>,
    pub removed: Vec<String>,
    pub before_source: Option<String>,
    pub after_source: Option<String>,
    pub history: Option<FileHistory>,
}

#[derive(Clone, Debug, Default)]
pub struct FileHistory {
    pub prior_commits: usize,
    pub prior_authors: usize,
}

impl ChangedFile {
    fn is_test_file(&self) -> bool {
        let path = self.path.to_ascii_lowercase();
        path.contains("/tests/")
            || path.starts_with("tests/")
            || path.ends_with("_test.go")
            || path.ends_with("_test.rs")
            || path.ends_with(".test.ts")
            || path.ends_with(".test.js")
            || path.ends_with(".spec.ts")
            || path.ends_with(".spec.js")
    }

    fn language(&self) -> &'static str {
        match Path::new(&self.path)
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or_default()
        {
            "rs" => "rust",
            "ts" => "typescript",
            "tsx" => "typescript",
            "js" => "javascript",
            "jsx" => "javascript",
            "go" => "go",
            "py" => "python",
            _ => "unknown",
        }
    }
}

#[derive(Clone, Debug)]
pub struct AnalysisContext {
    pub input_kind: InputKind,
    pub repo_root: Option<PathBuf>,
    pub base_rev: Option<String>,
    pub head_rev: Option<String>,
    pub files: Vec<ChangedFile>,
}

#[derive(Clone, Debug)]
pub struct PluginFinding {
    pub path: String,
    pub kind: ReasonKind,
    pub message: String,
    pub weight_override: Option<u32>,
    pub score_mode: PluginScoreMode,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PluginScoreMode {
    Base,
    Additive,
}

#[derive(Clone, Debug)]
pub struct PluginAnalysis {
    pub confidence: Confidence,
    pub findings: Vec<PluginFinding>,
}

impl PluginAnalysis {
    pub fn new(confidence: Confidence, findings: Vec<PluginFinding>) -> Self {
        Self {
            confidence,
            findings,
        }
    }
}

pub trait AnalyzerPlugin {
    fn id(&self) -> &'static str;
    fn analyze(&self, ctx: &AnalysisContext) -> PluginAnalysis;
}

#[derive(Clone, Debug, Serialize)]
pub struct FileScore {
    pub path: String,
    pub score: u32,
    pub language: String,
    pub base_score: u32,
    pub size_modifier: u32,
    pub hotspot_modifier: u32,
    pub plugin_contribution: u32,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct FeatureVector {
    pub files_changed: usize,
    pub public_signature_changes: usize,
    pub control_flow_changes: usize,
    pub assertion_changes: usize,
    pub size_signals: usize,
    pub hotspot_signals: usize,
    pub plugin_signals: usize,
}

#[derive(Clone, Debug, Serialize)]
pub struct ScoreReport {
    pub schema_version: String,
    pub scoring_model_version: String,
    pub score: u32,
    pub decision: Decision,
    pub confidence: Confidence,
    pub secondary_contribution: u32,
    pub reasons: Vec<Reason>,
    pub by_file: Vec<FileScore>,
    pub feature_vector: FeatureVector,
}

#[derive(Debug)]
struct GeneratedFileMatcher {
    rules: Vec<GeneratedRule>,
}

#[derive(Debug)]
struct GeneratedRule {
    matchers: Vec<GlobMatcher>,
    generated: bool,
}

#[derive(Clone, Debug, Default)]
struct FileScoreState {
    base_score: u32,
    size_modifier: u32,
    hotspot_modifier: u32,
    plugin_contribution: u32,
    has_semantic_risk: bool,
    score: u32,
}

impl FileScoreState {
    fn recompute_score(&mut self, max_score: u32) {
        self.score = self
            .base_score
            .saturating_add(self.size_modifier)
            .saturating_add(self.hotspot_modifier)
            .saturating_add(self.plugin_contribution)
            .min(max_score);
    }
}

#[derive(Clone, Debug)]
struct BaseScoring {
    base_score: u32,
    has_semantic_risk: bool,
    refactor_like: bool,
}

pub fn analyze_patch(
    patch: &str,
    plugins: &[&dyn AnalyzerPlugin],
) -> Result<ScoreReport, AnalyzeError> {
    analyze_patch_with_config(patch, plugins, &ScoreConfig::default_v1())
}

pub fn analyze_patch_with_config(
    patch: &str,
    plugins: &[&dyn AnalyzerPlugin],
    config: &ScoreConfig,
) -> Result<ScoreReport, AnalyzeError> {
    analyze_request_with_config(
        &AnalyzeRequest {
            input: AnalyzeInput::PatchText {
                patch: patch.to_string(),
            },
            repo_root: None,
        },
        plugins,
        config,
    )
}

pub fn analyze_request(
    request: &AnalyzeRequest,
    plugins: &[&dyn AnalyzerPlugin],
) -> Result<ScoreReport, AnalyzeError> {
    analyze_request_with_config(request, plugins, &ScoreConfig::default_v1())
}

pub fn analyze_request_with_config(
    request: &AnalyzeRequest,
    plugins: &[&dyn AnalyzerPlugin],
    config: &ScoreConfig,
) -> Result<ScoreReport, AnalyzeError> {
    let (ctx, _guard) = build_context(request, config)?;
    let mut reasons = Vec::new();
    let mut feature_vector = FeatureVector {
        files_changed: ctx.files.len(),
        ..FeatureVector::default()
    };
    let mut overall_confidence = Confidence::High;
    let mut file_scores = HashMap::new();

    for file in &ctx.files {
        file_scores.insert(
            file.path.clone(),
            score_file(file, &mut reasons, &mut feature_vector, config),
        );
    }

    for plugin in plugins {
        let analysis = plugin.analyze(&ctx);
        overall_confidence = min_confidence(&overall_confidence, &analysis.confidence);

        for finding in analysis.findings {
            apply_plugin_finding(
                finding,
                &mut file_scores,
                &mut reasons,
                &mut feature_vector,
                config,
            );
        }
    }

    let (by_file, aggregate_inputs) = collect_file_scores(&ctx.files, &file_scores);

    let (score, secondary_contribution) = aggregate_scores(&aggregate_inputs, &config.aggregation);

    Ok(ScoreReport {
        schema_version: config.schema_version.to_string(),
        scoring_model_version: config.scoring_model_version.clone(),
        score,
        decision: score_to_decision(score, &config.decision_thresholds),
        confidence: overall_confidence,
        secondary_contribution,
        reasons,
        by_file,
        feature_vector,
    })
}

struct WorkspaceGuard {
    paths: Vec<PathBuf>,
}

impl Drop for WorkspaceGuard {
    fn drop(&mut self) {
        for path in &self.paths {
            let _ = fs::remove_dir_all(path);
        }
    }
}

fn build_context(
    request: &AnalyzeRequest,
    config: &ScoreConfig,
) -> Result<(AnalysisContext, WorkspaceGuard), AnalyzeError> {
    match &request.input {
        AnalyzeInput::PatchText { patch } => {
            if patch.trim().is_empty() {
                return Err(AnalyzeError::EmptyPatch);
            }

            let files = filter_generated_files(
                parse_patch(patch)?,
                request.repo_root.as_deref(),
                &config.gitattributes_skip_attributes,
            )?;
            Ok((
                AnalysisContext {
                    input_kind: InputKind::PatchText,
                    repo_root: request.repo_root.clone(),
                    base_rev: None,
                    head_rev: None,
                    files,
                },
                WorkspaceGuard { paths: Vec::new() },
            ))
        }
        AnalyzeInput::GitRevisionRange {
            repo_root,
            base,
            head,
        } => {
            build_git_revision_context(repo_root, base, head, &config.gitattributes_skip_attributes)
        }
    }
}

fn build_git_revision_context(
    repo_root: &Path,
    base: &str,
    head: &str,
    gitattributes_skip_attributes: &[String],
) -> Result<(AnalysisContext, WorkspaceGuard), AnalyzeError> {
    let repo = Repository::open(repo_root)?;
    let base_commit = peel_commit(&repo, base)?;
    let head_commit = peel_commit(&repo, head)?;
    let base_tree = base_commit.tree()?;
    let head_tree = head_commit.tree()?;
    let patch = git_diff_patch(repo_root, base, head)?;
    let mut files = filter_generated_files(
        parse_patch(&patch)?,
        Some(repo_root),
        gitattributes_skip_attributes,
    )?;

    for file in &mut files {
        file.before_source = read_blob_text(&repo, &base_tree, file.old_path.as_deref())?;
        file.after_source = read_blob_text(&repo, &head_tree, file.new_path.as_deref())?;
        file.history = Some(read_file_history(repo_root, base, &file.path)?);
    }

    Ok((
        AnalysisContext {
            input_kind: InputKind::GitRevisionRange,
            repo_root: Some(repo_root.to_path_buf()),
            base_rev: Some(base.to_string()),
            head_rev: Some(head.to_string()),
            files,
        },
        WorkspaceGuard { paths: Vec::new() },
    ))
}

fn peel_commit<'repo>(
    repo: &'repo Repository,
    rev: &str,
) -> Result<git2::Commit<'repo>, AnalyzeError> {
    Ok(repo.revparse_single(rev)?.peel_to_commit()?)
}

fn git_diff_patch(repo_root: &Path, base: &str, head: &str) -> Result<String, AnalyzeError> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .arg("diff")
        .arg(base)
        .arg(head)
        .output()?;

    if !output.status.success() {
        return Err(AnalyzeError::Command(
            String::from_utf8_lossy(&output.stderr).trim().to_string(),
        ));
    }

    Ok(String::from_utf8(output.stdout)?)
}

fn read_blob_text(
    repo: &Repository,
    tree: &Tree<'_>,
    path: Option<&str>,
) -> Result<Option<String>, AnalyzeError> {
    let Some(path) = path else {
        return Ok(None);
    };

    if path == "/dev/null" {
        return Ok(None);
    }

    let entry = match tree.get_path(Path::new(path)) {
        Ok(entry) => entry,
        Err(_) => return Ok(None),
    };
    let object = entry.to_object(repo)?;
    let blob = object.peel_to_blob()?;
    Ok(String::from_utf8(blob.content().to_vec()).ok())
}

fn filter_generated_files(
    files: Vec<ChangedFile>,
    repo_root: Option<&Path>,
    skip_attributes: &[String],
) -> Result<Vec<ChangedFile>, AnalyzeError> {
    let Some(repo_root) = repo_root else {
        return Ok(files);
    };
    let Some(matcher) = load_generated_file_matcher(repo_root, skip_attributes)? else {
        return Ok(files);
    };

    Ok(files
        .into_iter()
        .filter(|file| !matcher.is_generated(&file.path))
        .collect())
}

fn load_generated_file_matcher(
    repo_root: &Path,
    skip_attributes: &[String],
) -> Result<Option<GeneratedFileMatcher>, AnalyzeError> {
    if skip_attributes.is_empty() {
        return Ok(None);
    }
    let path = repo_root.join(".gitattributes");
    if !path.exists() {
        return Ok(None);
    }

    let contents = fs::read_to_string(path)?;
    let skip_attributes: Vec<String> = skip_attributes
        .iter()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .collect();
    if skip_attributes.is_empty() {
        return Ok(None);
    }

    let mut rules = Vec::new();
    for line in contents.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        let mut parts = trimmed.split_whitespace();
        let Some(pattern) = parts.next() else {
            continue;
        };
        let Some(generated) = extract_skip_rule(parts.collect(), &skip_attributes) else {
            continue;
        };
        let matchers = compile_gitattributes_matchers(pattern)?;
        if matchers.is_empty() {
            continue;
        }

        rules.push(GeneratedRule {
            matchers,
            generated,
        });
    }

    if rules.is_empty() {
        return Ok(None);
    }

    Ok(Some(GeneratedFileMatcher { rules }))
}

fn extract_skip_rule(attributes: Vec<&str>, skip_attributes: &[String]) -> Option<bool> {
    let mut matched = None;

    for attribute in attributes {
        for target in skip_attributes {
            if let Some(enabled) = parse_gitattributes_attribute(attribute, target) {
                matched = Some(enabled);
            }
        }
    }

    matched
}

fn parse_gitattributes_attribute(attribute: &str, target: &str) -> Option<bool> {
    if attribute == target {
        return Some(true);
    }
    if attribute == format!("-{target}") || attribute == format!("!{target}") {
        return Some(false);
    }
    if let Some((name, value)) = attribute.split_once('=') {
        if name != target {
            return None;
        }
        return Some(!matches!(value, "false" | "unset"));
    }

    None
}

fn compile_gitattributes_matchers(pattern: &str) -> Result<Vec<GlobMatcher>, AnalyzeError> {
    let mut patterns = vec![pattern.to_string()];
    if !pattern.contains('/') {
        patterns.push(format!("**/{pattern}"));
    }

    patterns
        .into_iter()
        .map(|value| {
            Glob::new(&value)
                .map(|glob| glob.compile_matcher())
                .map_err(|error| {
                    AnalyzeError::Command(format!(
                        "invalid .gitattributes pattern `{value}`: {error}"
                    ))
                })
        })
        .collect()
}

impl GeneratedFileMatcher {
    fn is_generated(&self, path: &str) -> bool {
        let normalized = path.replace('\\', "/");
        let mut state = false;

        for rule in &self.rules {
            if rule
                .matchers
                .iter()
                .any(|matcher| matcher.is_match(&normalized))
            {
                state = rule.generated;
            }
        }

        state
    }
}

fn apply_plugin_finding(
    finding: PluginFinding,
    file_scores: &mut HashMap<String, FileScoreState>,
    reasons: &mut Vec<Reason>,
    feature_vector: &mut FeatureVector,
    config: &ScoreConfig,
) {
    let weight = finding
        .weight_override
        .unwrap_or_else(|| config.score_for(&finding.kind));
    let score = file_scores
        .entry(finding.path.clone())
        .or_insert_with(FileScoreState::default);

    match finding.score_mode {
        PluginScoreMode::Base => {
            score.base_score = score.base_score.max(weight);
        }
        PluginScoreMode::Additive => {
            score.plugin_contribution = score.plugin_contribution.saturating_add(weight);
        }
    }
    score.recompute_score(config.aggregation.max_score);

    feature_vector.plugin_signals += 1;
    reasons.push(
        finding
            .kind
            .as_reason(finding.path, weight, finding.message),
    );
}

fn collect_file_scores(
    files: &[ChangedFile],
    file_scores: &HashMap<String, FileScoreState>,
) -> (Vec<FileScore>, Vec<FileScoreState>) {
    let mut by_file = Vec::with_capacity(files.len());
    let mut aggregate_inputs = Vec::with_capacity(files.len());

    for file in files {
        let file_score = file_scores
            .get(&file.path)
            .cloned()
            .unwrap_or_else(FileScoreState::default);
        aggregate_inputs.push(file_score.clone());
        by_file.push(FileScore {
            path: file.path.clone(),
            score: file_score.score,
            language: file.language().to_string(),
            base_score: file_score.base_score,
            size_modifier: file_score.size_modifier,
            hotspot_modifier: file_score.hotspot_modifier,
            plugin_contribution: file_score.plugin_contribution,
        });
    }

    (by_file, aggregate_inputs)
}

fn score_file(
    file: &ChangedFile,
    reasons: &mut Vec<Reason>,
    feature_vector: &mut FeatureVector,
    config: &ScoreConfig,
) -> FileScoreState {
    if changed_lines(file)
        .iter()
        .all(|line| is_comment_or_blank(line))
    {
        return comment_only_score(file, reasons, config);
    }

    let base = detect_base_scoring(file, reasons, feature_vector, config);
    let (size_modifier, hotspot_modifier) =
        compute_modifiers(file, &base, reasons, feature_vector, config);
    let score =
        (base.base_score + size_modifier + hotspot_modifier).min(config.aggregation.max_score);

    FileScoreState {
        base_score: base.base_score,
        size_modifier,
        hotspot_modifier,
        plugin_contribution: 0,
        has_semantic_risk: base.has_semantic_risk,
        score,
    }
}

fn comment_only_score(
    file: &ChangedFile,
    reasons: &mut Vec<Reason>,
    config: &ScoreConfig,
) -> FileScoreState {
    let score = config.score_for(&ReasonKind::CommentOnly);
    reasons.push(ReasonKind::CommentOnly.as_reason(
        file.path.clone(),
        score,
        "comment or whitespace only change",
    ));

    FileScoreState {
        base_score: score,
        size_modifier: 0,
        hotspot_modifier: 0,
        plugin_contribution: 0,
        has_semantic_risk: false,
        score,
    }
}

fn detect_base_scoring(
    file: &ChangedFile,
    reasons: &mut Vec<Reason>,
    feature_vector: &mut FeatureVector,
    config: &ScoreConfig,
) -> BaseScoring {
    let refactor_like = is_refactor_like(file);
    let mut base_score = 0;
    let mut has_semantic_risk = false;

    if is_import_only(file) {
        let rule_score = config.score_for(&ReasonKind::ImportOnly);
        base_score = base_score.max(rule_score);
        reasons.push(ReasonKind::ImportOnly.as_reason(
            file.path.clone(),
            rule_score,
            "import or package declaration change",
        ));
    }

    if !file.is_test_file() && has_public_interface_change(file) {
        let public_interface_score = config.score_for(&ReasonKind::PublicInterfaceChange);
        let rule_score = if refactor_like {
            config.score_for(&ReasonKind::RefactorLikeChange)
        } else {
            public_interface_score
        };
        base_score = base_score.max(rule_score);
        has_semantic_risk = !refactor_like;
        feature_vector.public_signature_changes += 1;
        reasons.push(if refactor_like {
            ReasonKind::RefactorLikeChange.as_reason(
                file.path.clone(),
                rule_score,
                "public interface rename-like change",
            )
        } else {
            ReasonKind::PublicInterfaceChange.as_reason(
                file.path.clone(),
                rule_score,
                "public interface changed",
            )
        });
    }

    if let Some(control_flow_score) = control_flow_weight(file, config) {
        let rule_score = if file.is_test_file() {
            control_flow_score.min(20)
        } else {
            control_flow_score
        };
        base_score = base_score.max(rule_score);
        has_semantic_risk = true;
        feature_vector.control_flow_changes += 1;
        reasons.push(ReasonKind::ControlFlowChange.as_reason(
            file.path.clone(),
            rule_score,
            "control flow changed",
        ));
    }

    if file.is_test_file() && has_test_expectation_change(file) {
        let rule_score = config.score_for(&ReasonKind::TestExpectationChange);
        base_score = base_score.max(rule_score);
        has_semantic_risk = true;
        feature_vector.assertion_changes += 1;
        reasons.push(ReasonKind::TestExpectationChange.as_reason(
            file.path.clone(),
            rule_score,
            "test expectation changed",
        ));
    }

    if base_score == 0 && refactor_like {
        base_score = config.score_for(&ReasonKind::RefactorLikeChange);
        reasons.push(ReasonKind::RefactorLikeChange.as_reason(
            file.path.clone(),
            base_score,
            "refactor-like change",
        ));
    }

    if base_score == 0 {
        base_score = config.score_for(&ReasonKind::GenericCodeChange);
        reasons.push(ReasonKind::GenericCodeChange.as_reason(
            file.path.clone(),
            base_score,
            "generic code change",
        ));
    }

    BaseScoring {
        base_score,
        has_semantic_risk,
        refactor_like,
    }
}

fn compute_modifiers(
    file: &ChangedFile,
    base: &BaseScoring,
    reasons: &mut Vec<Reason>,
    feature_vector: &mut FeatureVector,
    config: &ScoreConfig,
) -> (u32, u32) {
    let modifier_cap = modifier_cap(base.base_score);
    let size_modifier =
        compute_size_modifier(file, base, modifier_cap, reasons, feature_vector, config);
    let hotspot_modifier = compute_hotspot_modifier(
        file,
        base,
        modifier_cap,
        size_modifier,
        reasons,
        feature_vector,
        config,
    );

    (size_modifier, hotspot_modifier)
}

fn compute_size_modifier(
    file: &ChangedFile,
    base: &BaseScoring,
    modifier_cap: u32,
    reasons: &mut Vec<Reason>,
    feature_vector: &mut FeatureVector,
    config: &ScoreConfig,
) -> u32 {
    if base.refactor_like {
        return 0;
    }

    let Some(rule_score) = change_size_weight(file, config) else {
        return 0;
    };
    let rule_score = if base.has_semantic_risk {
        rule_score
    } else {
        rule_score.div_ceil(3)
    };
    let size_modifier = rule_score.min(modifier_cap);
    if size_modifier == 0 {
        return 0;
    }

    feature_vector.size_signals += 1;
    reasons.push(ReasonKind::ChangeSize.as_reason(
        file.path.clone(),
        size_modifier,
        format!(
            "change size increased review load ({} changed lines)",
            change_volume(file)
        ),
    ));

    size_modifier
}

fn compute_hotspot_modifier(
    file: &ChangedFile,
    base: &BaseScoring,
    modifier_cap: u32,
    size_modifier: u32,
    reasons: &mut Vec<Reason>,
    feature_vector: &mut FeatureVector,
    config: &ScoreConfig,
) -> u32 {
    if base.refactor_like || !base.has_semantic_risk {
        return 0;
    }

    let Some(rule_score) = repo_hotspot_weight(file, config) else {
        return 0;
    };
    let hotspot_modifier = rule_score.min(modifier_cap.saturating_sub(size_modifier));
    if hotspot_modifier == 0 {
        return 0;
    }

    feature_vector.hotspot_signals += 1;
    let history = file.history.as_ref().expect("history should exist");
    reasons.push(ReasonKind::RepoHotspot.as_reason(
        file.path.clone(),
        hotspot_modifier,
        format!(
            "repo hotspot with {} prior commits across {} authors",
            history.prior_commits, history.prior_authors
        ),
    ));

    hotspot_modifier
}

fn aggregate_scores(scores: &[FileScoreState], config: &AggregationConfig) -> (u32, u32) {
    let mut sorted = scores.to_vec();
    sorted.sort_unstable_by(|left, right| right.score.cmp(&left.score));

    let top_score = sorted.first().map(|file| file.score).unwrap_or_default();
    let has_semantic_risk = sorted.iter().any(|file| file.has_semantic_risk);
    let secondary_raw: u32 = sorted.iter().skip(1).map(|file| file.score.min(30)).sum();
    let secondary_contribution = if has_semantic_risk {
        ((secondary_raw as f64) * config.secondary_ratio)
            .round()
            .min(config.secondary_cap as f64) as u32
    } else {
        0
    };

    (
        top_score
            .saturating_add(secondary_contribution)
            .min(config.max_score),
        secondary_contribution,
    )
}

fn score_to_decision(score: u32, config: &DecisionThresholds) -> Decision {
    if score <= config.skip_review_max {
        return Decision::SkipReview;
    }

    if score <= config.review_optional_max {
        return Decision::ReviewOptional;
    }

    if score <= config.review_suggested_max {
        return Decision::ReviewSuggested;
    }

    if score <= config.review_recommended_max {
        return Decision::ReviewRecommended;
    }

    Decision::ReviewRequired
}

fn min_confidence(left: &Confidence, right: &Confidence) -> Confidence {
    use Confidence::{High, Low, Medium};

    match (left, right) {
        (Low, _) | (_, Low) => Low,
        (Medium, _) | (_, Medium) => Medium,
        _ => High,
    }
}

fn parse_patch(patch: &str) -> Result<Vec<ChangedFile>, AnalyzeError> {
    let mut files = Vec::new();
    let mut current: Option<ChangedFile> = None;

    for line in patch.lines() {
        if let Some(rest) = line.strip_prefix("diff --git ") {
            if let Some(file) = current.take() {
                files.push(file);
            }

            let mut parts = rest.split_whitespace();
            let old_path = parts.next().map(strip_diff_prefix);
            let new_path = parts.next().map(strip_diff_prefix);
            current = Some(ChangedFile {
                path: new_path.clone().or(old_path.clone()).unwrap_or_default(),
                old_path,
                new_path,
                added: Vec::new(),
                removed: Vec::new(),
                before_source: None,
                after_source: None,
                history: None,
            });
            continue;
        }

        if let Some(path) = line.strip_prefix("--- ") {
            if let Some(file) = current.as_mut() {
                file.old_path = normalize_diff_path(path);
            }
            continue;
        }

        if let Some(path) = line.strip_prefix("+++ ") {
            if let Some(file) = current.as_mut() {
                file.new_path = normalize_diff_path(path);
                file.path = file
                    .new_path
                    .clone()
                    .or_else(|| file.old_path.clone())
                    .unwrap_or_default();
            }
            continue;
        }

        let Some(file) = current.as_mut() else {
            continue;
        };

        if line.starts_with("@@") {
            continue;
        }

        if let Some(value) = line.strip_prefix('+') {
            file.added.push(value.to_string());
            continue;
        }

        if let Some(value) = line.strip_prefix('-') {
            file.removed.push(value.to_string());
        }
    }

    if let Some(file) = current.take() {
        files.push(file);
    }

    files.retain(|file| {
        !file.path.is_empty() && (!file.added.is_empty() || !file.removed.is_empty())
    });

    if files.is_empty() {
        return Err(AnalyzeError::InvalidPatch);
    }

    Ok(files)
}

fn strip_diff_prefix(path: &str) -> String {
    path.strip_prefix("a/")
        .or_else(|| path.strip_prefix("b/"))
        .unwrap_or(path)
        .to_string()
}

fn normalize_diff_path(path: &str) -> Option<String> {
    if path == "/dev/null" {
        return None;
    }

    Some(strip_diff_prefix(path))
}

fn read_file_history(
    repo_root: &Path,
    base: &str,
    path: &str,
) -> Result<FileHistory, AnalyzeError> {
    let commit_count = git_stdout(repo_root, &["rev-list", "--count", base, "--", path])?;
    let author_log = git_stdout(repo_root, &["log", "--format=%an", base, "--", path])?;
    let prior_authors = author_log
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<std::collections::BTreeSet<_>>()
        .len();

    Ok(FileHistory {
        prior_commits: commit_count.trim().parse().unwrap_or_default(),
        prior_authors,
    })
}

fn git_stdout(repo_root: &Path, args: &[&str]) -> Result<String, AnalyzeError> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(args)
        .output()?;

    if !output.status.success() {
        return Err(AnalyzeError::Command(
            String::from_utf8_lossy(&output.stderr).trim().to_string(),
        ));
    }

    Ok(String::from_utf8(output.stdout)?)
}

fn changed_lines(file: &ChangedFile) -> Vec<&str> {
    file.added
        .iter()
        .chain(file.removed.iter())
        .map(String::as_str)
        .collect()
}

fn is_comment_or_blank(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed.is_empty()
        || trimmed.starts_with("//")
        || trimmed.starts_with('#')
        || trimmed.starts_with("/*")
        || trimmed.starts_with('*')
        || trimmed.starts_with("*/")
        || trimmed.starts_with("<!--")
}

fn is_import_line(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed.starts_with("use ")
        || trimmed.starts_with("import ")
        || trimmed.starts_with("from ")
        || trimmed.starts_with("include ")
        || trimmed.starts_with("package ")
}

fn is_import_only(file: &ChangedFile) -> bool {
    changed_lines(file)
        .iter()
        .all(|line| is_import_line(line) || is_comment_or_blank(line))
}

fn has_public_interface_change(file: &ChangedFile) -> bool {
    if is_internal_or_private_path(&file.path) {
        return false;
    }

    changed_lines(file)
        .iter()
        .any(|line| is_public_signature_line(line))
}

fn is_public_signature_line(line: &str) -> bool {
    let trimmed = line.trim();

    if trimmed.starts_with("pub ") || is_public_export_signature_line(trimmed) {
        return true;
    }

    if let Some(rest) = trimmed.strip_prefix("func ") {
        return rest
            .chars()
            .next()
            .is_some_and(|ch| ch.is_ascii_uppercase());
    }

    false
}

fn is_public_export_signature_line(line: &str) -> bool {
    [
        "export function ",
        "export async function ",
        "export class ",
        "export interface ",
        "export type ",
        "export enum ",
        "export default function ",
        "export default async function ",
        "export default class ",
    ]
    .iter()
    .any(|prefix| line.starts_with(prefix))
        || is_function_like_exported_const(line)
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

fn control_flow_weight(file: &ChangedFile, config: &ScoreConfig) -> Option<u32> {
    let has_control_flow = changed_lines(file).iter().any(|line| {
        let trimmed = line.trim();
        [
            "if ",
            "if(",
            "else if",
            "match ",
            "switch ",
            "for ",
            "while ",
            "throw ",
            "break",
            "continue",
        ]
        .iter()
        .any(|keyword| trimmed.starts_with(keyword))
    });
    if !has_control_flow {
        return None;
    }

    let base_weight = config.score_for(&ReasonKind::ControlFlowChange);
    let is_heavy = changed_lines(file).iter().any(|line| {
        let trimmed = line.trim();
        trimmed.starts_with("throw ")
            || trimmed.starts_with("for ")
            || trimmed.starts_with("while ")
            || trimmed.starts_with("match ")
            || trimmed.starts_with("switch ")
    });

    if is_heavy {
        return Some(base_weight);
    }

    let nesting_bonus = match approximate_branch_nesting_depth(file) {
        depth if depth >= 3 => 15,
        2 => 10,
        _ => 0,
    };

    Some((base_weight.min(30) + nesting_bonus).min(base_weight))
}

fn approximate_branch_nesting_depth(file: &ChangedFile) -> usize {
    let mut current_depth = 0usize;
    let mut max_depth = 0usize;

    for line in &file.added {
        let trimmed = line.trim();

        let closing_braces = trimmed.chars().filter(|ch| *ch == '}').count();
        current_depth = current_depth.saturating_sub(closing_braces);

        if starts_branch(trimmed) {
            let branch_depth = current_depth + 1;
            max_depth = max_depth.max(branch_depth);
        }

        let opening_braces = trimmed.chars().filter(|ch| *ch == '{').count();
        current_depth += opening_braces;
    }

    max_depth
}

fn starts_branch(trimmed: &str) -> bool {
    [
        "if ", "if(", "else if", "match ", "switch ", "for ", "while ", "select ", "select{",
        "select {",
    ]
    .iter()
    .any(|keyword| trimmed.starts_with(keyword))
}

fn has_test_expectation_change(file: &ChangedFile) -> bool {
    let removed = normalized_test_oracle_lines(&file.removed);
    let added = normalized_test_oracle_lines(&file.added);

    if removed.is_empty() && added.is_empty() {
        return false;
    }

    removed != added
}

pub(crate) fn normalized_test_oracle_lines(lines: &[String]) -> Vec<String> {
    let mut normalized: Vec<String> = lines
        .iter()
        .filter_map(|line| normalize_test_oracle_line(line))
        .collect();
    normalized.sort_unstable();
    normalized
}

fn normalize_test_oracle_line(line: &str) -> Option<String> {
    let trimmed = line.trim();
    if !(trimmed.contains("assert.")
        || trimmed.contains("require.")
        || trimmed.contains("expect(")
        || trimmed.contains("snapshot")
        || trimmed.contains("cmp.Diff("))
    {
        return None;
    }

    if trimmed.contains("assert.") || trimmed.contains("require.") {
        return Some(normalize_assert_like_line(trimmed));
    }

    Some(trimmed.to_string())
}

fn normalize_assert_like_line(line: &str) -> String {
    let Some(open_paren) = line.find('(') else {
        return line.to_string();
    };
    let Some(close_paren) = line.rfind(')') else {
        return line.to_string();
    };

    let callee = line[..open_paren].trim();
    let args = split_top_level_args(&line[open_paren + 1..close_paren]);
    if args.is_empty() {
        return line.to_string();
    }

    let semantic_arg_count = match args.len() {
        0..=3 => args.len(),
        _ => 3,
    };
    let semantic_args = args
        .into_iter()
        .take(semantic_arg_count)
        .map(|arg| arg.trim().to_string())
        .collect::<Vec<_>>()
        .join(", ");

    format!("{callee}({semantic_args})")
}

fn split_top_level_args(input: &str) -> Vec<&str> {
    let mut args = Vec::new();
    let mut start = 0usize;
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;

    for (index, ch) in input.char_indices() {
        if in_string {
            if escaped {
                escaped = false;
                continue;
            }
            match ch {
                '\\' => escaped = true,
                '"' => in_string = false,
                _ => {}
            }
            continue;
        }

        match ch {
            '"' => in_string = true,
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                args.push(input[start..index].trim());
                start = index + 1;
            }
            _ => {}
        }
    }

    let tail = input[start..].trim();
    if !tail.is_empty() {
        args.push(tail);
    }

    args
}

fn is_refactor_like(file: &ChangedFile) -> bool {
    let mut removed = normalized_code_lines(&file.removed);
    let mut added = normalized_code_lines(&file.added);

    if removed.is_empty() || added.is_empty() {
        return false;
    }

    removed.sort_unstable();
    added.sort_unstable();

    removed == added
}

fn change_volume(file: &ChangedFile) -> usize {
    file.added.len() + file.removed.len()
}

fn modifier_cap(base_score: u32) -> u32 {
    match base_score {
        0..=10 => 0,
        11..=29 => 8,
        30..=54 => 12,
        _ => 18,
    }
}

fn change_size_weight(file: &ChangedFile, config: &ScoreConfig) -> Option<u32> {
    if config.score_for(&ReasonKind::ChangeSize) == 0 {
        return None;
    }

    let changed_lines = change_volume(file);
    let changed_ratio = file
        .after_source
        .as_deref()
        .or(file.before_source.as_deref())
        .map(|source| changed_lines as f64 / source.lines().count().max(1) as f64);
    let ratio_can_raise_to_high = changed_lines >= 6;

    let mut score: u32 = if changed_lines >= 25
        || (ratio_can_raise_to_high && changed_ratio.is_some_and(|ratio| ratio >= 0.6))
    {
        12
    } else if changed_lines >= 10
        || (ratio_can_raise_to_high && changed_ratio.is_some_and(|ratio| ratio >= 0.3))
    {
        8
    } else if changed_lines >= 4 {
        4
    } else {
        0
    };

    if file.is_test_file() {
        score = (score + 1) / 2;
    }

    (score > 0).then_some(score)
}

fn repo_hotspot_weight(file: &ChangedFile, config: &ScoreConfig) -> Option<u32> {
    let history = file.history.as_ref()?;
    if config.score_for(&ReasonKind::RepoHotspot) == 0 {
        return None;
    }

    let mut score = if history.prior_commits >= 10 {
        8
    } else if history.prior_commits >= 4 {
        4
    } else {
        0
    };

    if file.is_test_file() {
        score = score.min(3);
    }

    (score > 0).then_some(score)
}

fn normalized_code_lines(lines: &[String]) -> Vec<String> {
    lines
        .iter()
        .map(String::as_str)
        .filter(|line| !is_comment_or_blank(line))
        .map(normalize_line)
        .filter(|line| !line.is_empty())
        .collect()
}

fn normalize_line(line: &str) -> String {
    let mut normalized = String::new();
    let mut chars = line.trim().chars().peekable();

    while let Some(ch) = chars.next() {
        if ch.is_ascii_whitespace() {
            continue;
        }

        if ch == '"' || ch == '\'' {
            normalized.push_str("\"s\"");
            while let Some(next) = chars.next() {
                if next == ch {
                    break;
                }
                if next == '\\' {
                    let _ = chars.next();
                }
            }
            continue;
        }

        if ch.is_ascii_alphabetic() || ch == '_' {
            normalized.push('x');
            while let Some(next) = chars.peek() {
                if next.is_ascii_alphanumeric() || *next == '_' {
                    let _ = chars.next();
                } else {
                    break;
                }
            }
            continue;
        }

        if ch.is_ascii_digit() {
            normalized.push('0');
            while let Some(next) = chars.peek() {
                if next.is_ascii_digit() {
                    let _ = chars.next();
                } else {
                    break;
                }
            }
            continue;
        }

        normalized.push(ch);
    }

    normalized
}

pub fn resolve_builtin_plugins(
    names: &[String],
) -> Result<Vec<Box<dyn AnalyzerPlugin>>, AnalyzeError> {
    let mut plugins: Vec<Box<dyn AnalyzerPlugin>> = Vec::new();

    for name in names {
        match name.as_str() {
            "go" => plugins.push(Box::new(plugins::go::GoPlugin::new())),
            "ts" => plugins.push(Box::new(plugins::typescript::TypeScriptPlugin::new())),
            unknown => {
                return Err(AnalyzeError::Command(format!("unknown plugin: {unknown}")));
            }
        }
    }

    Ok(plugins)
}
