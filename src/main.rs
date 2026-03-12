use std::{fs, io::Read, path::PathBuf, process::ExitCode};

use clap::Parser;
use git2::{DiffFormat, Repository};
use shiwake::{ScoreConfig, analyze_patch, analyze_patch_with_config};

#[derive(Debug, Parser)]
struct Cli {
    #[arg(long)]
    repo: Option<PathBuf>,
    #[arg(long)]
    patch: Option<PathBuf>,
    #[arg(long)]
    config: Option<PathBuf>,
    #[arg(long)]
    base: Option<String>,
    #[arg(long)]
    head: Option<String>,
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("ERROR: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let cli = Cli::parse();
    let patch = match (&cli.patch, &cli.base, &cli.head) {
        (Some(path), None, None) => read_patch(path),
        (None, Some(base), Some(head)) => {
            let repo_path = cli
                .repo
                .as_deref()
                .ok_or_else(|| String::from("--repo is required with --base/--head"))?;
            patch_from_repo(repo_path, base, head)
        }
        _ => Err(String::from("use either --patch or --base with --head")),
    }?;

    let report = if let Some(config_path) = &cli.config {
        let config_text = fs::read_to_string(config_path).map_err(|error| error.to_string())?;
        let config = ScoreConfig::from_toml(&config_text).map_err(|error| error.to_string())?;
        analyze_patch_with_config(&patch, &[], &config).map_err(|error| error.to_string())?
    } else {
        analyze_patch(&patch, &[]).map_err(|error| error.to_string())?
    };
    let json = serde_json::to_string(&report).map_err(|error| error.to_string())?;
    println!("{json}");
    Ok(())
}

fn read_patch(path: &PathBuf) -> Result<String, String> {
    if path.as_os_str() == "-" {
        let mut buffer = String::new();
        std::io::stdin()
            .read_to_string(&mut buffer)
            .map_err(|error| error.to_string())?;
        return Ok(buffer);
    }

    fs::read_to_string(path).map_err(|error| error.to_string())
}

fn patch_from_repo(repo_path: &std::path::Path, base: &str, head: &str) -> Result<String, String> {
    let repo = Repository::open(repo_path).map_err(|error| error.to_string())?;
    let base_object = repo
        .revparse_single(base)
        .map_err(|error| error.to_string())?;
    let head_object = repo
        .revparse_single(head)
        .map_err(|error| error.to_string())?;
    let base_commit = base_object
        .peel_to_commit()
        .map_err(|error| error.to_string())?;
    let head_commit = head_object
        .peel_to_commit()
        .map_err(|error| error.to_string())?;
    let base_tree = base_commit.tree().map_err(|error| error.to_string())?;
    let head_tree = head_commit.tree().map_err(|error| error.to_string())?;
    let diff = repo
        .diff_tree_to_tree(Some(&base_tree), Some(&head_tree), None)
        .map_err(|error| error.to_string())?;
    let mut buffer = Vec::new();
    diff.print(DiffFormat::Patch, |_delta, _hunk, line| {
        buffer.extend_from_slice(line.content());
        true
    })
    .map_err(|error| error.to_string())?;

    String::from_utf8(buffer).map_err(|error| error.to_string())
}
