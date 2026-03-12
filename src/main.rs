use std::{fs, io::Read, path::PathBuf, process::ExitCode};

use clap::Parser;
use shiwake::{
    AnalyzeInput, AnalyzeRequest, ScoreConfig, analyze_request, analyze_request_with_config,
    resolve_builtin_plugins,
};

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
    #[arg(long = "plugin")]
    plugins: Vec<String>,
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
    let request = match (&cli.patch, &cli.base, &cli.head) {
        (Some(path), None, None) => AnalyzeRequest {
            input: AnalyzeInput::PatchText {
                patch: read_patch(path)?,
            },
            repo_root: cli.repo.clone(),
        },
        (None, Some(base), Some(head)) => AnalyzeRequest {
            input: AnalyzeInput::GitRevisionRange {
                repo_root: cli
                    .repo
                    .clone()
                    .ok_or_else(|| String::from("--repo is required with --base/--head"))?,
                base: base.clone(),
                head: head.clone(),
            },
            repo_root: cli.repo.clone(),
        },
        _ => return Err(String::from("use either --patch or --base with --head")),
    };

    let plugin_storage =
        resolve_builtin_plugins(&cli.plugins).map_err(|error| error.to_string())?;
    let plugins: Vec<&dyn shiwake::AnalyzerPlugin> = plugin_storage
        .iter()
        .map(|plugin| plugin.as_ref())
        .collect();

    let report = if let Some(config_path) = &cli.config {
        let config_text = fs::read_to_string(config_path).map_err(|error| error.to_string())?;
        let config = ScoreConfig::from_toml(&config_text).map_err(|error| error.to_string())?;
        analyze_request_with_config(&request, &plugins, &config)
            .map_err(|error| error.to_string())?
    } else {
        analyze_request(&request, &plugins).map_err(|error| error.to_string())?
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
