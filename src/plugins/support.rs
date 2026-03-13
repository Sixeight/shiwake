use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use crate::{
    AnalysisContext, ChangedFile, Confidence, PluginAnalysis, PluginFinding, PluginScoreMode,
    ReasonKind,
};

static PLUGIN_TEMP_DIR_COUNTER: AtomicU64 = AtomicU64::new(0);

pub fn changed_files_with_extension(ctx: &AnalysisContext, extension: &str) -> Vec<String> {
    changed_files_matching(ctx, |path| path.ends_with(extension))
}

pub fn changed_files_matching<F>(ctx: &AnalysisContext, mut include: F) -> Vec<String>
where
    F: FnMut(&str) -> bool,
{
    ctx.files
        .iter()
        .filter(|file| include(&file.path))
        .map(|file| file.path.clone())
        .collect()
}

pub fn base_finding(path: String, kind: ReasonKind, message: impl Into<String>) -> PluginFinding {
    PluginFinding {
        path,
        kind,
        message: message.into(),
        weight_override: None,
        score_mode: PluginScoreMode::Base,
    }
}

pub fn weighted_base_finding(
    path: String,
    kind: ReasonKind,
    message: impl Into<String>,
    weight_override: u32,
) -> PluginFinding {
    PluginFinding {
        path,
        kind,
        message: message.into(),
        weight_override: Some(weight_override),
        score_mode: PluginScoreMode::Base,
    }
}

pub fn additive_finding(
    path: String,
    kind: ReasonKind,
    message: impl Into<String>,
) -> PluginFinding {
    PluginFinding {
        path,
        kind,
        message: message.into(),
        weight_override: None,
        score_mode: PluginScoreMode::Additive,
    }
}

pub fn fallback_analysis<F>(
    ctx: &AnalysisContext,
    extension: &str,
    fallback_kind: ReasonKind,
    reason: &str,
    enrich: F,
) -> PluginAnalysis
where
    F: FnMut(&ChangedFile, &mut Vec<PluginFinding>),
{
    fallback_analysis_matching(
        ctx,
        |path| path.ends_with(extension),
        fallback_kind,
        reason,
        enrich,
    )
}

pub fn fallback_analysis_matching<Include, Enrich>(
    ctx: &AnalysisContext,
    mut include: Include,
    fallback_kind: ReasonKind,
    reason: &str,
    mut enrich: Enrich,
) -> PluginAnalysis
where
    Include: FnMut(&str) -> bool,
    Enrich: FnMut(&ChangedFile, &mut Vec<PluginFinding>),
{
    let mut findings = Vec::new();

    for file in &ctx.files {
        if !include(&file.path) {
            continue;
        }

        findings.push(additive_finding(
            file.path.clone(),
            fallback_kind.clone(),
            reason.to_string(),
        ));
        enrich(file, &mut findings);
    }

    PluginAnalysis::new(Confidence::Medium, findings)
}

pub fn unique_temp_dir(prefix: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("time should move forward")
        .as_nanos();
    let counter = PLUGIN_TEMP_DIR_COUNTER.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "shiwake-{prefix}-{}-{nanos}-{counter}",
        std::process::id()
    ));
    fs::create_dir_all(&path).expect("temp helper dir should be created");
    path
}
