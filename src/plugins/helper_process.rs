use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use serde::{Serialize, de::DeserializeOwned};

use crate::{AnalysisContext, InputKind, PluginAnalysis, ReasonKind};

use super::support::unique_temp_dir;

pub struct EmbeddedHelper<'a> {
    pub temp_dir_prefix: &'a str,
    pub files: &'a [(&'a str, &'a str)],
    pub program: &'a str,
    pub args: &'a [&'a str],
}

pub struct RevisionHelperInputs {
    pub changed_files: Vec<String>,
    pub before_workspace: PathBuf,
    pub after_workspace: PathBuf,
}

pub struct RevisionHelperFallback<'a> {
    pub kind: ReasonKind,
    pub input_kind_reason: &'a str,
    pub before_workspace_reason: &'a str,
    pub after_workspace_reason: &'a str,
    pub required_files_reason: &'a str,
}

pub fn write_embedded_files(dir: &Path, files: &[(&str, &str)]) -> Result<(), String> {
    for (relative_path, contents) in files {
        fs::write(dir.join(relative_path), contents).map_err(|error| error.to_string())?;
    }

    Ok(())
}

pub fn run_json_command<Request, Response>(
    program: &str,
    args: &[&str],
    current_dir: &Path,
    request: &Request,
) -> Result<Response, String>
where
    Request: Serialize,
    Response: DeserializeOwned,
{
    let request_json = serde_json::to_vec(request).map_err(|error| error.to_string())?;

    let mut child = Command::new(program)
        .args(args)
        .current_dir(current_dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| error.to_string())?;

    if let Some(stdin) = child.stdin.as_mut() {
        stdin
            .write_all(&request_json)
            .map_err(|error| error.to_string())?;
    }

    let output = child
        .wait_with_output()
        .map_err(|error| error.to_string())?;

    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }

    serde_json::from_slice(&output.stdout).map_err(|error| error.to_string())
}

pub fn resolve_revision_helper_inputs<F>(
    ctx: &AnalysisContext,
    extension: &str,
    required_files: &[&str],
    fallback: RevisionHelperFallback<'_>,
    enrich: F,
) -> Result<RevisionHelperInputs, PluginAnalysis>
where
    F: FnMut(&crate::ChangedFile, &mut Vec<crate::PluginFinding>),
{
    resolve_revision_helper_inputs_matching(
        ctx,
        |path| path.ends_with(extension),
        required_files,
        fallback,
        enrich,
    )
}

pub fn resolve_revision_helper_inputs_matching<Include, Enrich>(
    ctx: &AnalysisContext,
    include: Include,
    required_files: &[&str],
    fallback: RevisionHelperFallback<'_>,
    enrich: Enrich,
) -> Result<RevisionHelperInputs, PluginAnalysis>
where
    Include: Copy + Fn(&str) -> bool,
    Enrich: FnMut(&crate::ChangedFile, &mut Vec<crate::PluginFinding>),
{
    let changed_files = super::support::changed_files_matching(ctx, include);
    if changed_files.is_empty() {
        return Ok(RevisionHelperInputs {
            changed_files,
            before_workspace: PathBuf::new(),
            after_workspace: PathBuf::new(),
        });
    }

    if ctx.input_kind != InputKind::GitRevisionRange {
        return Err(super::support::fallback_analysis_matching(
            ctx,
            include,
            fallback.kind.clone(),
            fallback.input_kind_reason,
            enrich,
        ));
    }

    let Some(before_workspace) = &ctx.before_workspace else {
        return Err(super::support::fallback_analysis_matching(
            ctx,
            include,
            fallback.kind.clone(),
            fallback.before_workspace_reason,
            enrich,
        ));
    };
    let Some(after_workspace) = &ctx.after_workspace else {
        return Err(super::support::fallback_analysis_matching(
            ctx,
            include,
            fallback.kind.clone(),
            fallback.after_workspace_reason,
            enrich,
        ));
    };

    if required_files
        .iter()
        .any(|path| !before_workspace.join(path).exists())
        || required_files
            .iter()
            .any(|path| !after_workspace.join(path).exists())
    {
        return Err(super::support::fallback_analysis_matching(
            ctx,
            include,
            fallback.kind,
            fallback.required_files_reason,
            enrich,
        ));
    }

    Ok(RevisionHelperInputs {
        changed_files,
        before_workspace: before_workspace.clone(),
        after_workspace: after_workspace.clone(),
    })
}

pub fn run_embedded_json_helper<Request, Response>(
    helper: &EmbeddedHelper<'_>,
    request: &Request,
) -> Result<Response, String>
where
    Request: Serialize,
    Response: DeserializeOwned,
{
    let helper_dir = unique_temp_dir(helper.temp_dir_prefix);
    write_embedded_files(&helper_dir, helper.files)?;

    let response =
        run_json_command::<_, Response>(helper.program, helper.args, &helper_dir, request);
    let _ = fs::remove_dir_all(&helper_dir);
    response
}
