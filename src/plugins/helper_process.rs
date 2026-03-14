use std::{
    fs,
    hash::{Hash, Hasher},
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use serde::{Serialize, de::DeserializeOwned};

use crate::{AnalysisContext, InputKind, PluginAnalysis, ReasonKind};

pub struct EmbeddedHelper<'a> {
    pub temp_dir_prefix: &'a str,
    pub files: &'a [(&'a str, &'a str)],
    pub program: &'a str,
    pub args: &'a [&'a str],
}

pub struct RevisionHelperInputs {
    pub changed_files: Vec<String>,
    pub repo_root: PathBuf,
    pub base_rev: String,
    pub head_rev: String,
}

pub struct RevisionHelperFallback<'a> {
    pub kind: ReasonKind,
    pub input_kind_reason: &'a str,
    pub repo_root_reason: &'a str,
    pub base_rev_reason: &'a str,
    pub head_rev_reason: &'a str,
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
    args: &[String],
    current_dir: &Path,
    request: &Request,
) -> Result<Response, String>
where
    Request: Serialize,
    Response: DeserializeOwned,
{
    let request_json = serde_json::to_vec(request).map_err(|error| error.to_string())?;

    let mut command = Command::new(program);
    command
        .args(args)
        .current_dir(current_dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if program == "go" {
        let go_cache_dir = current_dir.join(".gocache");
        fs::create_dir_all(&go_cache_dir).map_err(|error| error.to_string())?;
        command.env("GOCACHE", go_cache_dir);
    }

    let mut child = command.spawn().map_err(|error| error.to_string())?;

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
            repo_root: PathBuf::new(),
            base_rev: String::new(),
            head_rev: String::new(),
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

    let Some(repo_root) = &ctx.repo_root else {
        return Err(super::support::fallback_analysis_matching(
            ctx,
            include,
            fallback.kind.clone(),
            fallback.repo_root_reason,
            enrich,
        ));
    };
    let Some(base_rev) = &ctx.base_rev else {
        return Err(super::support::fallback_analysis_matching(
            ctx,
            include,
            fallback.kind.clone(),
            fallback.base_rev_reason,
            enrich,
        ));
    };
    let Some(head_rev) = &ctx.head_rev else {
        return Err(super::support::fallback_analysis_matching(
            ctx,
            include,
            fallback.kind.clone(),
            fallback.head_rev_reason,
            enrich,
        ));
    };

    if required_files
        .iter()
        .any(|path| !revision_path_exists(repo_root, base_rev, path))
        || required_files
            .iter()
            .any(|path| !revision_path_exists(repo_root, head_rev, path))
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
        repo_root: repo_root.clone(),
        base_rev: base_rev.clone(),
        head_rev: head_rev.clone(),
    })
}

fn revision_path_exists(repo_root: &Path, rev: &str, path: &str) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .arg("cat-file")
        .arg("-e")
        .arg(format!("{rev}:{path}"))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

pub fn run_embedded_json_helper<Request, Response>(
    helper: &EmbeddedHelper<'_>,
    request: &Request,
) -> Result<Response, String>
where
    Request: Serialize,
    Response: DeserializeOwned,
{
        let helper_dir = ensure_embedded_helper_dir(helper)?;
    let (program, args) = resolve_helper_command(helper, &helper_dir)?;

    run_json_command::<_, Response>(&program, &args, &helper_dir, request)
}

fn ensure_embedded_helper_dir(helper: &EmbeddedHelper<'_>) -> Result<PathBuf, String> {
    let helper_dir = embedded_helper_dir(helper);
    if helper_dir.exists() {
        return Ok(helper_dir);
    }

    fs::create_dir_all(&helper_dir).map_err(|error| error.to_string())?;
    write_embedded_files(&helper_dir, helper.files)?;
    Ok(helper_dir)
}

fn embedded_helper_dir(helper: &EmbeddedHelper<'_>) -> PathBuf {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    helper.temp_dir_prefix.hash(&mut hasher);
    helper.program.hash(&mut hasher);
    helper.args.hash(&mut hasher);
    for (relative_path, contents) in helper.files {
        relative_path.hash(&mut hasher);
        contents.hash(&mut hasher);
    }
    let fingerprint = hasher.finish();
    std::env::temp_dir().join(format!("shiwake-helper-{}-{fingerprint:x}", helper.temp_dir_prefix))
}

fn resolve_helper_command(
    helper: &EmbeddedHelper<'_>,
    helper_dir: &Path,
) -> Result<(String, Vec<String>), String> {
    if helper.program == "go" && helper.args == ["run", "."] {
        return ensure_go_helper_binary(helper_dir).map(|path| {
            (
                path.to_string_lossy().to_string(),
                Vec::<String>::new(),
            )
        });
    }

    Ok((
        helper.program.to_string(),
        helper.args.iter().map(|arg| (*arg).to_string()).collect(),
    ))
}

fn ensure_go_helper_binary(helper_dir: &Path) -> Result<PathBuf, String> {
    let binary_path = helper_dir.join("helper-bin");
    if binary_path.exists() {
        return Ok(binary_path);
    }

    let go_cache_dir = helper_dir.join(".gocache");
    fs::create_dir_all(&go_cache_dir).map_err(|error| error.to_string())?;

    let output = Command::new("go")
        .arg("build")
        .arg("-o")
        .arg(&binary_path)
        .arg(".")
        .current_dir(helper_dir)
        .env("GOCACHE", &go_cache_dir)
        .output()
        .map_err(|error| error.to_string())?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }

    Ok(binary_path)
}

#[cfg(test)]
mod tests {
    use super::{EmbeddedHelper, embedded_helper_dir, resolve_helper_command};

    #[test]
    fn embedded_helper_dir_is_stable_for_same_helper() {
        let helper = EmbeddedHelper {
            temp_dir_prefix: "ts-helper",
            files: &[("main.js", "console.log('x')")],
            program: "node",
            args: &["main.js"],
        };

        assert_eq!(embedded_helper_dir(&helper), embedded_helper_dir(&helper));
    }

    #[test]
    fn embedded_helper_dir_changes_when_contents_change() {
        let first = EmbeddedHelper {
            temp_dir_prefix: "ts-helper",
            files: &[("main.js", "console.log('x')")],
            program: "node",
            args: &["main.js"],
        };
        let second = EmbeddedHelper {
            temp_dir_prefix: "ts-helper",
            files: &[("main.js", "console.log('y')")],
            program: "node",
            args: &["main.js"],
        };

        assert_ne!(embedded_helper_dir(&first), embedded_helper_dir(&second));
    }

    #[test]
    fn resolve_helper_command_keeps_non_go_helpers_unchanged() {
        let helper = EmbeddedHelper {
            temp_dir_prefix: "ts-helper",
            files: &[("main.js", "console.log('x')")],
            program: "node",
            args: &["main.js"],
        };

        let (program, args) =
            resolve_helper_command(&helper, std::path::Path::new("/tmp")).expect("command");

        assert_eq!(program, "node");
        assert_eq!(args, vec![String::from("main.js")]);
    }
}
