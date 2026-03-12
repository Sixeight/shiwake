use std::{fmt, path::Path};

use serde::{Deserialize, Serialize};

const SCORING_MODEL_VERSION: &str = "v1";

#[derive(Debug)]
pub enum AnalyzeError {
    EmptyPatch,
    InvalidPatch,
    InvalidConfig(toml::de::Error),
}

impl fmt::Display for AnalyzeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyPatch => f.write_str("patch is empty"),
            Self::InvalidPatch => f.write_str("patch does not contain any changed files"),
            Self::InvalidConfig(error) => write!(f, "invalid config: {error}"),
        }
    }
}

impl std::error::Error for AnalyzeError {}

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

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[derive(Deserialize)]
pub enum ReasonKind {
    CommentOnly,
    ImportOnly,
    RefactorLikeChange,
    PublicInterfaceChange,
    ControlFlowChange,
    TestExpectationChange,
    GenericCodeChange,
    PluginSignal,
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
            ],
        }
    }

    pub fn from_toml(input: &str) -> Result<Self, AnalyzeError> {
        toml::from_str(input).map_err(AnalyzeError::InvalidConfig)
    }

    fn score_for(&self, kind: &ReasonKind) -> u32 {
        self.rules
            .iter()
            .find(|rule| &rule.kind == kind)
            .map(|rule| rule.score)
            .unwrap_or_default()
    }
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

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct Reason {
    pub kind: ReasonKind,
    pub file: String,
    pub weight: u32,
    pub message: String,
}

#[derive(Clone, Debug)]
pub struct ChangedFile {
    pub path: String,
    pub added: Vec<String>,
    pub removed: Vec<String>,
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

pub trait AnalyzerPlugin {
    fn id(&self) -> &'static str;
    fn supports(&self, file: &ChangedFile) -> bool;
    fn analyze(&self, file: &ChangedFile) -> AnalysisResult;
}

#[derive(Clone, Debug)]
pub struct AnalysisResult {
    pub file: String,
    pub score_delta: u32,
    pub confidence: Confidence,
    pub reasons: Vec<Reason>,
}

impl AnalysisResult {
    pub fn from_plugin(
        file: String,
        score_delta: u32,
        confidence: Confidence,
        reasons: Vec<Reason>,
    ) -> Self {
        Self {
            file,
            score_delta,
            confidence,
            reasons,
        }
    }
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
    if patch.trim().is_empty() {
        return Err(AnalyzeError::EmptyPatch);
    }

    let files = parse_patch(patch)?;
    let mut by_file = Vec::with_capacity(files.len());
    let mut reasons = Vec::new();
    let mut feature_vector = FeatureVector {
        files_changed: files.len(),
        ..FeatureVector::default()
    };
    let mut overall_confidence = Confidence::High;
    let mut file_scores = Vec::with_capacity(files.len());

    for file in files {
        let mut file_score = score_file(&file, &mut reasons, &mut feature_vector, config);

        for plugin in plugins {
            if !plugin.supports(&file) {
                continue;
            }

            let result = plugin.analyze(&file);
            file_score = file_score.saturating_add(result.score_delta);
            feature_vector.plugin_signals += result.reasons.len();
            overall_confidence = min_confidence(&overall_confidence, &result.confidence);
            reasons.extend(result.reasons);
        }

        file_score = file_score.min(config.aggregation.max_score);
        file_scores.push(file_score);
        by_file.push(FileScore {
            path: file.path.clone(),
            score: file_score,
            language: file.language().to_string(),
        });
    }

    let score = aggregate_scores(&file_scores, &config.aggregation);

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
        if line.starts_with("diff --git ") {
            if let Some(file) = current.take() {
                files.push(file);
            }

            current = Some(ChangedFile {
                path: String::new(),
                added: Vec::new(),
                removed: Vec::new(),
            });

            continue;
        }

        if let Some(path) = line.strip_prefix("+++ b/") {
            let file = current.get_or_insert_with(|| ChangedFile {
                path: String::new(),
                added: Vec::new(),
                removed: Vec::new(),
            });
            file.path = path.to_string();
            continue;
        }

        let Some(file) = current.as_mut() else {
            continue;
        };

        if line.starts_with("@@") {
            continue;
        }

        if line.starts_with("+++ ") || line.starts_with("--- ") {
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
