pub mod config;
pub mod copy;
mod copy_fast_path;
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
use plan::{FileContext, PlanBuild, PlanJob, TransferPlan};
use progress_format::{render_live_screen, render_post_run_screen};
use progress_model::{
    CategoryRowModel, ErrorRowModel, LiveScreenModel, PostRunScreenModel, ProgressBarModel,
    SummaryMetric, WorkerRowModel,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreviewUiMode {
    Live,
    PostCopy,
    All,
}

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
    pub preview_ui: Option<PreviewUiMode>,
}

pub fn run(options: RunOptions) -> Result<(), PathsyncError> {
    if let Some(mode) = options.preview_ui {
        print!("{}", preview_ui_output(mode));
        return Ok(());
    }

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
    let plan_build = build_transfer_plan_with_stats(&job, options.force)?;
    let plans = plan_build.plans;

    if plans.is_empty() {
        println!("no new files to copy for job `{}`", job.name);
        return Ok(());
    }

    if options.dry_run {
        copy::print_dry_run(&job, &plans);
        return Ok(());
    }

    Ok(copy::run_copy(&job, plans, plan_build.stats)?)
}

pub fn preview_ui_output(mode: PreviewUiMode) -> String {
    let live = render_live_screen(&preview_live_screen_model()).join("\n");
    let post = render_post_run_screen(&preview_post_run_screen_model()).join("\n");

    match mode {
        PreviewUiMode::Live => format!("{live}\n"),
        PreviewUiMode::PostCopy => format!("{post}\n"),
        PreviewUiMode::All => format!("{live}\n\n{post}\n"),
    }
}

pub fn build_transfer_plan(
    job: &ResolvedJob,
    force: bool,
) -> Result<Vec<TransferPlan>, PathsyncError> {
    Ok(build_transfer_plan_with_stats(job, force)?.plans)
}

pub fn build_transfer_plan_with_stats(
    job: &ResolvedJob,
    force: bool,
) -> Result<PlanBuild, PathsyncError> {
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

fn preview_live_screen_model() -> LiveScreenModel {
    LiveScreenModel {
        job_name: "vlog-sync".to_string(),
        status: "LIVE / COPY-LARGE".to_string(),
        summary: vec![
            SummaryMetric::new("Scanned", "2,941"),
            SummaryMetric::new("Planned", "318"),
            SummaryMetric::new("Copied", "141"),
            SummaryMetric::new("Failed", "1"),
            SummaryMetric::new("Bytes", "58.2 GB / 133.0 GB"),
            SummaryMetric::new("Rate", "142.4 MB/s"),
            SummaryMetric::new("Elapsed", "7m08s"),
            SummaryMetric::new("ETA", "8m46s"),
        ],
        overall_label: "Total copy progress".to_string(),
        overall_progress: ProgressBarModel::new(43, 30),
        overall_progress_text: "58.2 GB / 133.0 GB".to_string(),
        phase_label: "overall  copying large files".to_string(),
        workers: vec![
            WorkerRowModel::active(
                '⠋',
                "W01",
                64,
                "A001_C014_0101AB.MP4",
                "8.2 GB",
                "78.4 MB/s",
            ),
            WorkerRowModel::active(
                '⠙',
                "W02",
                51,
                "A001_C015_0101AB.MP4",
                "7.9 GB",
                "64.0 MB/s",
            ),
            WorkerRowModel::active('⠹', "W03", 12, "GX010193.MP4", "2.1 GB", "41.8 MB/s"),
            WorkerRowModel::idle("W04"),
        ],
    }
}

fn preview_post_run_screen_model() -> PostRunScreenModel {
    PostRunScreenModel {
        job_name: "vlog-sync".to_string(),
        status: "COMPLETE WITH ERRORS".to_string(),
        summary: vec![
            SummaryMetric::new("Scanned", "2,941"),
            SummaryMetric::new("Planned", "318"),
            SummaryMetric::new("Copied", "316"),
            SummaryMetric::new("Failed", "2"),
            SummaryMetric::new("Bytes transferred", "131.6 GB"),
            SummaryMetric::new("Avg rate", "121.7 MB/s"),
            SummaryMetric::new("Elapsed", "18m01s"),
            SummaryMetric::new("Skip rate", "89.2%"),
        ],
        completion_label: "Copy completion".to_string(),
        completion_progress: ProgressBarModel::new(99, 30),
        categories: vec![
            CategoryRowModel::new("skipped existing", 2623, "0 B", "100.0%", "0.0s"),
            CategoryRowModel::new("copied mp4", 204, "128.4 GB", "67.1%", "16m09s"),
            CategoryRowModel::new("copied jpg", 112, "3.2 GB", "72.8%", "1m04s"),
            CategoryRowModel::new("failed permission", 1, "14.2 MB", "0.0%", "--"),
            CategoryRowModel::new("failed collision", 1, "8.7 MB", "0.0%", "--"),
        ],
        errors: vec![
            ErrorRowModel::new("[local] GX010193.MP4", "permission denied"),
            ErrorRowModel::new(
                "[local] GX010194.MP4",
                "destination collision after layout render",
            ),
        ],
    }
}
