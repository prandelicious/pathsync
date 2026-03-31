use std::fs;

use pathsync::config;
use pathsync::error::{ConfigError, PathsyncError};
use pathsync::policy::{ComparePolicy, TimezonePolicy, TransferPolicy};
use pathsync::{
    PreviewUiMode, RunOptions, build_transfer_plan, build_transfer_plan_with_stats,
    preview_ui_output,
};

#[test]
fn public_policy_types_are_exposed_through_resolved_jobs() {
    let root = std::env::temp_dir().join(format!("pathsync-public-api-{}", std::process::id()));
    let source = root.join("source");
    let target = root.join("target");
    fs::create_dir_all(&source).unwrap();
    fs::create_dir_all(&target).unwrap();

    let config = config::Config {
        default_job: Some("sync".to_string()),
        parallel: None,
        timezone: Some("UTC".to_string()),
        jobs: [(
            "sync".to_string(),
            config::JobConfig {
                enabled: Some(true),
                source: source.clone(),
                target: target.clone(),
                extensions: vec!["jpg".to_string()],
                compare: Some(config::CompareConfig {
                    mode: Some("path".to_string()),
                }),
                transfer: Some(config::TransferConfig {
                    mode: Some("adaptive".to_string()),
                    large_file_threshold_mb: Some(100),
                    large_file_slots: Some(2),
                }),
                parallel: Some(2),
                timezone: None,
                layout: config::LayoutConfig::Preset("flat".to_string()),
            },
        )]
        .into_iter()
        .collect(),
    };

    let job = config::resolve_job(&config, None, None, false, None).unwrap();

    assert_eq!(job.compare_policy, ComparePolicy::Path);
    assert_eq!(
        job.transfer_policy,
        TransferPolicy::Adaptive {
            large_file_threshold_bytes: 100 * 1024 * 1024,
            large_file_slots: 2,
        }
    );
    assert_eq!(job.timezone_policy, TimezonePolicy::Utc);
}

#[test]
fn public_build_transfer_plan_returns_typed_errors() {
    let root = std::env::temp_dir().join(format!(
        "pathsync-public-api-collision-{}",
        std::process::id()
    ));
    let source = root.join("source");
    let target = root.join("target");
    fs::create_dir_all(source.join("one")).unwrap();
    fs::create_dir_all(source.join("two")).unwrap();
    fs::create_dir_all(&target).unwrap();
    fs::write(source.join("one/photo.jpg"), b"1111").unwrap();
    fs::write(source.join("two/photo.jpg"), b"2222").unwrap();

    let config = config::Config {
        default_job: Some("sync".to_string()),
        parallel: None,
        timezone: None,
        jobs: [(
            "sync".to_string(),
            config::JobConfig {
                enabled: Some(true),
                source: source.clone(),
                target: target.clone(),
                extensions: vec!["jpg".to_string()],
                compare: None,
                transfer: None,
                parallel: None,
                timezone: None,
                layout: config::LayoutConfig::Detailed(config::LayoutDetailed {
                    kind: "template".to_string(),
                    value: Some("{filename}".to_string()),
                }),
            },
        )]
        .into_iter()
        .collect(),
    };

    let job = config::resolve_job(&config, None, None, false, None).unwrap();
    let err = build_transfer_plan(&job, false).unwrap_err();

    assert!(matches!(
        err,
        PathsyncError::Plan(pathsync::plan::PlanError::Collision { .. })
    ));
}

#[test]
fn public_build_transfer_plan_with_stats_returns_planning_metrics() {
    let root =
        std::env::temp_dir().join(format!("pathsync-public-api-stats-{}", std::process::id()));
    let source = root.join("source");
    let target = root.join("target");
    fs::create_dir_all(&source).unwrap();
    fs::create_dir_all(&target).unwrap();
    fs::write(source.join("one.jpg"), b"1111").unwrap();
    fs::write(source.join("two.jpg"), b"2222").unwrap();
    fs::write(target.join("two.jpg"), b"2222").unwrap();

    let config = config::Config {
        default_job: Some("sync".to_string()),
        parallel: None,
        timezone: None,
        jobs: [(
            "sync".to_string(),
            config::JobConfig {
                enabled: Some(true),
                source: source.clone(),
                target: target.clone(),
                extensions: vec!["jpg".to_string()],
                compare: Some(config::CompareConfig {
                    mode: Some("size_mtime".to_string()),
                }),
                transfer: None,
                parallel: None,
                timezone: None,
                layout: config::LayoutConfig::Preset("flat".to_string()),
            },
        )]
        .into_iter()
        .collect(),
    };

    let job = config::resolve_job(&config, None, None, false, None).unwrap();
    let build = build_transfer_plan_with_stats(&job, false).unwrap();

    assert_eq!(build.stats.scanned_files, 2);
    assert_eq!(build.stats.planned_files, 1);
    assert_eq!(build.stats.planned_bytes, 4);
    assert_eq!(build.stats.skipped_existing_files, 1);
    assert_eq!(build.stats.skipped_existing_bytes, 4);
    assert_eq!(build.plans.len(), 1);
    assert!(build_transfer_plan(&job, false).unwrap().len() == 1);
}

#[test]
fn public_run_returns_typed_config_errors() {
    let err = pathsync::run(RunOptions {
        config: Some("/definitely/missing/pathsync.toml".into()),
        ..RunOptions::default()
    })
    .unwrap_err();

    assert!(matches!(
        err,
        PathsyncError::Config(ConfigError::ReadConfig { .. })
    ));
}

#[test]
fn public_preview_ui_output_can_render_live_and_post_copy_screens() {
    let live = preview_ui_output(PreviewUiMode::Live);
    let post = preview_ui_output(PreviewUiMode::PostCopy);
    let all = preview_ui_output(PreviewUiMode::All);

    assert!(live.contains("LIVE / COPY-LARGE"));
    assert!(!live.contains("COMPLETE WITH ERRORS"));
    assert!(post.contains("COMPLETE WITH ERRORS"));
    assert!(!post.contains("LIVE / COPY-LARGE"));
    assert!(all.contains("LIVE / COPY-LARGE"));
    assert!(all.contains("COMPLETE WITH ERRORS"));
}
