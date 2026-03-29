pub mod config;
pub mod copy;
pub mod date;
pub mod error;
pub mod format;
pub mod plan;
pub mod policy;
pub mod progress_format;
pub mod progress_model;

use std::fs;
use std::path::{Path, PathBuf};

use config::{CompareConfig, Config, LayoutConfig, ResolvedJob, TransferConfig};
use error::PathsyncError;
use plan::{FileContext, PlanJob, TransferPlan};

#[derive(Debug, Clone, Default)]
pub struct RunOptions {
    pub config: Option<PathBuf>,
    pub list_jobs: bool,
    pub dry_run: bool,
    pub force: bool,
    pub parallel: Option<usize>,
    pub allow_disabled: bool,
    pub extensions: Option<Vec<String>>,
    pub job: Option<String>,
}

pub fn run(options: RunOptions) -> Result<(), PathsyncError> {
    let config_path = options.config.unwrap_or_else(config::default_config_path);
    let config = config::load_config(&config_path)?;
    if options.list_jobs {
        print_jobs(&config);
        return Ok(());
    }

    let job = config::resolve_job(
        &config,
        options.job.as_deref(),
        options.parallel,
        options.allow_disabled,
        options.extensions.as_deref(),
    )?;
    let plans = build_transfer_plan(&job, options.force)?;

    if plans.is_empty() {
        println!("no new files to copy for job `{}`", job.name);
        return Ok(());
    }

    if options.dry_run {
        copy::print_dry_run(&job, &plans);
        return Ok(());
    }

    Ok(copy::run_copy(&job, plans)?)
}

pub fn build_transfer_plan(
    job: &ResolvedJob,
    force: bool,
) -> Result<Vec<TransferPlan>, PathsyncError> {
    let plan_job = PlanJob {
        source: job.source.clone(),
        target: job.target.clone(),
        extensions: job.extensions.clone(),
        compare_policy: job.compare_policy,
        template: job.template.clone(),
    };

    plan::build_plan(&plan_job, force, |source, metadata| {
        build_file_context(&job.source, source, metadata, &job.timezone_policy).map_err(|err| {
            plan::PlanError::Io {
                context: "failed to build file context".to_string(),
                path: Some(source.to_path_buf()),
                message: err.to_string(),
            }
        })
    })
    .map_err(PathsyncError::from)
}

fn build_file_context(
    source_root: &Path,
    source: &Path,
    metadata: &fs::Metadata,
    timezone_policy: &policy::TimezonePolicy,
) -> Result<FileContext, PathsyncError> {
    let filename = source
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| PathsyncError::InvalidSourceFileName {
            path: source.to_path_buf(),
        })?
        .to_string();
    let stem = source
        .file_stem()
        .and_then(|value| value.to_str())
        .ok_or_else(|| PathsyncError::InvalidSourceFileStem {
            path: source.to_path_buf(),
        })?
        .to_string();
    let ext = plan::extension_of(source).ok_or_else(|| PathsyncError::MissingFileExtension {
        path: source.to_path_buf(),
    })?;
    let source_rel_dir = plan::relative_source_dir(source_root, source)?;
    let modified = metadata
        .modified()
        .map_err(|source_err| PathsyncError::ReadModifiedTime {
            path: source.to_path_buf(),
            source: source_err,
        })?;
    let (year, month, day) = date::extract_date_parts(source, modified, timezone_policy)?;

    Ok(FileContext {
        year,
        month,
        day,
        ext,
        stem,
        filename,
        source_rel_dir,
    })
}

fn print_jobs(config: &Config) {
    for (name, job) in &config.jobs {
        println!("{name}");
        println!("  enabled    : {}", job.enabled.unwrap_or(true));
        println!("  source     : {}", job.source.display());
        println!("  target     : {}", job.target.display());
        println!(
            "  extensions : {}",
            config::normalize_extensions(&job.extensions).join(", ")
        );
        println!(
            "  compare    : {}",
            compare_config_summary(job.compare.as_ref())
        );
        println!("  layout     : {}", layout_summary(&job.layout));
        println!(
            "  transfer   : {}",
            transfer_config_summary(job.transfer.as_ref())
        );
        println!(
            "  timezone   : {}",
            job.timezone
                .as_deref()
                .or(config.timezone.as_deref())
                .unwrap_or("local")
        );
        println!(
            "  parallel   : {}",
            job.parallel.or(config.parallel).unwrap_or(4)
        );
        println!();
    }
}

fn compare_config_summary(compare: Option<&CompareConfig>) -> String {
    compare
        .and_then(|compare| compare.mode.clone())
        .unwrap_or_else(|| "size_mtime".to_string())
}

fn layout_summary(layout: &LayoutConfig) -> String {
    match layout {
        LayoutConfig::Preset(name) => name.clone(),
        LayoutConfig::Detailed(detail) => match detail.kind.as_str() {
            "template" => format!(
                "template: {}",
                detail.value.as_deref().unwrap_or("<missing>")
            ),
            other => other.to_string(),
        },
    }
}

fn transfer_config_summary(transfer: Option<&TransferConfig>) -> String {
    let Some(transfer) = transfer else {
        return "standard".to_string();
    };

    match transfer.mode.as_deref().unwrap_or("standard") {
        "adaptive" => format!(
            "adaptive (large >= {} MB)",
            transfer.large_file_threshold_mb.unwrap_or(100)
        ),
        other => other.to_string(),
    }
}
