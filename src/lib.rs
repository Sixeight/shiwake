use std::{
    collections::HashMap,
    fmt, fs,
    path::{Path, PathBuf},
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

use git2::{Repository, Tree};
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
    ReviewRecommended,
    ReviewRequired,
}

impl Decision {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::SkipReview => "skip_review",
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
    RefactorLikeChange,
    PublicInterfaceChange,
    ControlFlowChange,
    TestExpectationChange,
    GenericCodeChange,
    PluginSignal,
    GoExportedApiChange,
    GoInterfaceBreak,
    GoConcurrencyChange,
    GoAnalysisFallback,
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
    pub review_recommended_max: u32,
}

#[derive(Clone, Debug, Deserialize)]
pub struct AggregationConfig {
    pub top_file_weight: f64,
    pub secondary_file_weight: f64,
    pub max_score: u32,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ScoreConfig {
    pub schema_version: u32,
    pub scoring_model_version: String,
    pub decision_thresholds: DecisionThresholds,
    pub aggregation: AggregationConfig,
    pub rules: Vec<RuleConfig>,
}

impl ScoreConfig {
    pub fn default_v1() -> Self {
        Self {
            schema_version: 1,
            scoring_model_version: SCORING_MODEL_VERSION.to_string(),
            decision_thresholds: DecisionThresholds {
                skip_review_max: 24,
                review_recommended_max: 59,
            },
            aggregation: AggregationConfig {
                top_file_weight: 1.0,
                secondary_file_weight: 0.33,
                max_score: 100,
            },
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
                    kind: ReasonKind::GoAnalysisFallback,
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
    pub before_workspace: Option<PathBuf>,
    pub after_workspace: Option<PathBuf>,
    pub files: Vec<ChangedFile>,
}

#[derive(Clone, Debug)]
pub struct PluginFinding {
    pub path: String,
    pub kind: ReasonKind,
    pub message: String,
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
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct FeatureVector {
    pub files_changed: usize,
    pub public_signature_changes: usize,
    pub control_flow_changes: usize,
    pub assertion_changes: usize,
    pub plugin_signals: usize,
}

#[derive(Clone, Debug, Serialize)]
pub struct ScoreReport {
    pub schema_version: String,
    pub scoring_model_version: String,
    pub score: u32,
    pub decision: Decision,
    pub confidence: Confidence,
    pub reasons: Vec<Reason>,
    pub by_file: Vec<FileScore>,
    pub feature_vector: FeatureVector,
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
    let (ctx, _guard) = build_context(request)?;
    let mut reasons = Vec::new();
    let mut feature_vector = FeatureVector {
        files_changed: ctx.files.len(),
        ..FeatureVector::default()
    };
    let mut overall_confidence = Confidence::High;
    let mut file_scores = HashMap::new();

    for file in &ctx.files {
        let score = score_file(file, &mut reasons, &mut feature_vector, config);
        file_scores.insert(file.path.clone(), score);
    }

    for plugin in plugins {
        let analysis = plugin.analyze(&ctx);
        overall_confidence = min_confidence(&overall_confidence, &analysis.confidence);

        for finding in analysis.findings {
            let weight = config.score_for(&finding.kind);
            let score = file_scores.entry(finding.path.clone()).or_insert(0);
            *score = score.saturating_add(weight);
            feature_vector.plugin_signals += 1;
            reasons.push(
                finding
                    .kind
                    .as_reason(finding.path, weight, finding.message),
            );
        }
    }

    let mut by_file = Vec::with_capacity(ctx.files.len());
    let mut aggregate_inputs = Vec::with_capacity(ctx.files.len());
    for file in &ctx.files {
        let file_score = file_scores
            .get(&file.path)
            .copied()
            .unwrap_or_default()
            .min(config.aggregation.max_score);
        aggregate_inputs.push(file_score);
        by_file.push(FileScore {
            path: file.path.clone(),
            score: file_score,
            language: file.language().to_string(),
        });
    }

    let score = aggregate_scores(&aggregate_inputs, &config.aggregation);

    Ok(ScoreReport {
        schema_version: config.schema_version.to_string(),
        scoring_model_version: config.scoring_model_version.clone(),
        score,
        decision: score_to_decision(score, &config.decision_thresholds),
        confidence: overall_confidence,
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
) -> Result<(AnalysisContext, WorkspaceGuard), AnalyzeError> {
    match &request.input {
        AnalyzeInput::PatchText { patch } => {
            if patch.trim().is_empty() {
                return Err(AnalyzeError::EmptyPatch);
            }

            let files = parse_patch(patch)?;
            Ok((
                AnalysisContext {
                    input_kind: InputKind::PatchText,
                    repo_root: request.repo_root.clone(),
                    base_rev: None,
                    head_rev: None,
                    before_workspace: None,
                    after_workspace: None,
                    files,
                },
                WorkspaceGuard { paths: Vec::new() },
            ))
        }
        AnalyzeInput::GitRevisionRange {
            repo_root,
            base,
            head,
        } => build_git_revision_context(repo_root, base, head),
    }
}

fn build_git_revision_context(
    repo_root: &Path,
    base: &str,
    head: &str,
) -> Result<(AnalysisContext, WorkspaceGuard), AnalyzeError> {
    let repo = Repository::open(repo_root)?;
    let base_commit = peel_commit(&repo, base)?;
    let head_commit = peel_commit(&repo, head)?;
    let base_tree = base_commit.tree()?;
    let head_tree = head_commit.tree()?;
    let patch = git_diff_patch(repo_root, base, head)?;
    let mut files = parse_patch(&patch)?;

    for file in &mut files {
        file.before_source = read_blob_text(&repo, &base_tree, file.old_path.as_deref())?;
        file.after_source = read_blob_text(&repo, &head_tree, file.new_path.as_deref())?;
    }

    let before_workspace = unique_temp_dir("go-before");
    let after_workspace = unique_temp_dir("go-after");
    export_tree(&repo, &base_tree, &before_workspace)?;
    export_tree(&repo, &head_tree, &after_workspace)?;

    Ok((
        AnalysisContext {
            input_kind: InputKind::GitRevisionRange,
            repo_root: Some(repo_root.to_path_buf()),
            base_rev: Some(base.to_string()),
            head_rev: Some(head.to_string()),
            before_workspace: Some(before_workspace.clone()),
            after_workspace: Some(after_workspace.clone()),
            files,
        },
        WorkspaceGuard {
            paths: vec![before_workspace, after_workspace],
        },
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

fn unique_temp_dir(prefix: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("time should move forward")
        .as_nanos();
    let path = std::env::temp_dir().join(format!("shiwake-{prefix}-{nanos}"));
    fs::create_dir_all(&path).expect("temp directory should be created");
    path
}

fn export_tree(repo: &Repository, tree: &Tree<'_>, dest: &Path) -> Result<(), AnalyzeError> {
    tree.walk(git2::TreeWalkMode::PreOrder, |root, entry| {
        let Some(name) = entry.name() else {
            return git2::TreeWalkResult::Ok;
        };

        let relative = if root.is_empty() {
            PathBuf::from(name)
        } else {
            Path::new(root).join(name)
        };
        let target = dest.join(&relative);

        match entry.kind() {
            Some(git2::ObjectType::Tree) => {
                let _ = fs::create_dir_all(&target);
            }
            Some(git2::ObjectType::Blob) => {
                if let Some(parent) = target.parent() {
                    let _ = fs::create_dir_all(parent);
                }
                if let Ok(blob) = repo.find_blob(entry.id()) {
                    let _ = fs::write(&target, blob.content());
                }
            }
            _ => {}
        }

        git2::TreeWalkResult::Ok
    })?;

    Ok(())
}

fn score_file(
    file: &ChangedFile,
    reasons: &mut Vec<Reason>,
    feature_vector: &mut FeatureVector,
    config: &ScoreConfig,
) -> u32 {
    if changed_lines(file)
        .iter()
        .all(|line| is_comment_or_blank(line))
    {
        let score = config.score_for(&ReasonKind::CommentOnly);
        reasons.push(ReasonKind::CommentOnly.as_reason(
            file.path.clone(),
            score,
            "comment or whitespace only change",
        ));
        return score;
    }

    let mut score = 0;

    if is_import_only(file) {
        let rule_score = config.score_for(&ReasonKind::ImportOnly);
        score = score.max(rule_score);
        reasons.push(ReasonKind::ImportOnly.as_reason(
            file.path.clone(),
            rule_score,
            "import or package declaration change",
        ));
    }

    if has_public_interface_change(file) {
        let rule_score = config.score_for(&ReasonKind::PublicInterfaceChange);
        score = score.max(rule_score);
        feature_vector.public_signature_changes += 1;
        reasons.push(ReasonKind::PublicInterfaceChange.as_reason(
            file.path.clone(),
            rule_score,
            "public interface changed",
        ));
    }

    if has_control_flow_change(file) {
        let rule_score = config.score_for(&ReasonKind::ControlFlowChange);
        score = score.max(rule_score);
        feature_vector.control_flow_changes += 1;
        reasons.push(ReasonKind::ControlFlowChange.as_reason(
            file.path.clone(),
            rule_score,
            "control flow changed",
        ));
    }

    if file.is_test_file() && has_test_expectation_change(file) {
        let rule_score = config.score_for(&ReasonKind::TestExpectationChange);
        score = score.max(rule_score);
        feature_vector.assertion_changes += 1;
        reasons.push(ReasonKind::TestExpectationChange.as_reason(
            file.path.clone(),
            rule_score,
            "test expectation changed",
        ));
    }

    if score == 0 && is_refactor_like(file) {
        score = config.score_for(&ReasonKind::RefactorLikeChange);
        reasons.push(ReasonKind::RefactorLikeChange.as_reason(
            file.path.clone(),
            score,
            "refactor-like change",
        ));
    }

    if score == 0 {
        score = config.score_for(&ReasonKind::GenericCodeChange);
        reasons.push(ReasonKind::GenericCodeChange.as_reason(
            file.path.clone(),
            score,
            "generic code change",
        ));
    }

    score
}

fn aggregate_scores(scores: &[u32], config: &AggregationConfig) -> u32 {
    let mut sorted = scores.to_vec();
    sorted.sort_unstable_by(|left, right| right.cmp(left));

    let mut total = 0.0;
    for (index, score) in sorted.iter().enumerate() {
        if index == 0 {
            total += (*score as f64) * config.top_file_weight;
        } else {
            total += (*score as f64) * config.secondary_file_weight;
        }
    }

    total.round().clamp(0.0, config.max_score as f64) as u32
}

fn score_to_decision(score: u32, config: &DecisionThresholds) -> Decision {
    if score <= config.skip_review_max {
        return Decision::SkipReview;
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
    changed_lines(file)
        .iter()
        .any(|line| is_public_signature_line(line))
}

fn is_public_signature_line(line: &str) -> bool {
    let trimmed = line.trim();

    if trimmed.starts_with("pub ") || trimmed.starts_with("export ") {
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

fn has_control_flow_change(file: &ChangedFile) -> bool {
    changed_lines(file).iter().any(|line| {
        let trimmed = line.trim();
        [
            "if ", "if(", "else if", "match ", "switch ", "for ", "while ", "return ", "throw ",
        ]
        .iter()
        .any(|keyword| trimmed.starts_with(keyword))
    })
}

fn has_test_expectation_change(file: &ChangedFile) -> bool {
    file.added.iter().chain(file.removed.iter()).any(|line| {
        let trimmed = line.trim();
        trimmed.contains("assert")
            || trimmed.contains("expect(")
            || trimmed.contains("snapshot")
            || trimmed.contains("require.")
    })
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
            unknown => {
                return Err(AnalyzeError::Command(format!("unknown plugin: {unknown}")));
            }
        }
    }

    Ok(plugins)
}
