use anyhow::Result;
use clap::{Parser, ValueEnum};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(name = "pathsync")]
#[command(about = "Config-driven file sync with template-based target layout")]
#[command(version)]
struct Cli {
    #[arg(long, value_enum, help = "Render a canned UI preview and exit")]
    preview_ui: Option<PreviewUiArg>,

    #[arg(long, help = "Path to the TOML configuration file")]
    config: Option<PathBuf>,

    #[arg(long, help = "List configured jobs and exit")]
    list_jobs: bool,

    #[arg(long, help = "Print planned copies without writing files")]
    dry_run: bool,

    #[arg(long, help = "Copy files even when the compare policy would skip them")]
    force: bool,

    #[arg(long, help = "Override the configured parallel worker count")]
    parallel: Option<usize>,

    #[arg(long, help = "Allow execution of a disabled job")]
    allow_disabled: bool,

    #[arg(
        long,
        value_delimiter = ',',
        help = "Override the configured extension allow-list with a comma-separated list"
    )]
    extensions: Option<Vec<String>>,

    #[arg(
        value_name = "JOB",
        help = "Job name to run; uses default_job when omitted"
    )]
    job: Option<String>,
}

#[derive(Clone, Debug, ValueEnum)]
enum PreviewUiArg {
    Live,
    PostCopy,
    All,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    Ok(pathsync::run(pathsync::RunOptions {
        preview_ui: cli.preview_ui.map(|mode| match mode {
            PreviewUiArg::Live => pathsync::PreviewUiMode::Live,
            PreviewUiArg::PostCopy => pathsync::PreviewUiMode::PostCopy,
            PreviewUiArg::All => pathsync::PreviewUiMode::All,
        }),
        config: cli.config,
        list_jobs: cli.list_jobs,
        dry_run: cli.dry_run,
        force: cli.force,
        parallel: cli.parallel,
        allow_disabled: cli.allow_disabled,
        extensions: cli.extensions,
        job: cli.job,
    })?)
}
