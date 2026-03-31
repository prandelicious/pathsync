use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::SystemTime;

use filetime::{FileTime, set_file_mtime};
use pathsync::error::PathsyncError;
use pathsync::{build_transfer_plan, config};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(prefix: &str) -> Self {
        let unique = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .expect("system time before unix epoch")
            .as_nanos();
        let counter = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "{prefix}-{unique}-{}-{counter}",
            std::process::id()
        ));
        fs::create_dir_all(&path).expect("failed to create temp dir");
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn write_file(path: &Path, contents: &[u8]) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("failed to create parent directories");
    }
    let mut file = File::create(path).expect("failed to create file");
    file.write_all(contents).expect("failed to write file");
}

fn write_config(
    root: &TempDir,
    source: &Path,
    target: &Path,
    compare_mode: &str,
    layout: &str,
) -> PathBuf {
    let config_path = root.path().join("config.toml");
    let text = format!(
        r#"
default_job = "sync"

[jobs.sync]
enabled = true
source = "{source}"
target = "{target}"
extensions = ["jpg"]
compare = {{ mode = "{compare_mode}" }}
transfer = {{ mode = "standard" }}
layout = {layout}
"#,
        source = source.display(),
        target = target.display(),
        compare_mode = compare_mode,
        layout = layout,
    );
    fs::write(&config_path, text).expect("failed to write config");
    config_path
}

fn run_pathsync(args: &[&str]) -> CommandOutput {
    run_pathsync_with_env(args, &[])
}

fn run_pathsync_with_env(args: &[&str], extra_env: &[(&str, &str)]) -> CommandOutput {
    let mut command = Command::new(env!("CARGO_BIN_EXE_pathsync"));
    command
        .env("NO_COLOR", "1")
        .env("TERM", "dumb")
        .env("COLUMNS", "120")
        .env("LC_ALL", "C")
        .args(args);

    for (key, value) in extra_env {
        command.env(key, value);
    }

    let output = command.output().expect("failed to run pathsync");
    CommandOutput {
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        status: output.status,
    }
}

struct CommandOutput {
    stdout: String,
    stderr: String,
    status: std::process::ExitStatus,
}

#[test]
fn version_flag_prints_binary_version() {
    let output = run_pathsync(&["--version"]);

    assert!(
        output.status.success(),
        "stdout={}\nstderr={}",
        output.stdout,
        output.stderr
    );
    assert_eq!(output.stderr, "");
    assert_eq!(
        output.stdout.trim(),
        format!("pathsync {}", env!("CARGO_PKG_VERSION"))
    );
}

#[test]
fn preview_ui_flag_renders_canned_live_and_post_copy_screens_without_config() {
    let live = run_pathsync(&["--preview-ui", "live"]);
    let post = run_pathsync(&["--preview-ui", "post-copy"]);
    let all = run_pathsync(&["--preview-ui", "all"]);

    assert!(
        live.status.success(),
        "stdout={}\nstderr={}",
        live.stdout,
        live.stderr
    );
    assert!(live.stdout.contains("LIVE / COPY-LARGE"));
    assert!(!live.stdout.contains("COMPLETE WITH ERRORS"));

    assert!(
        post.status.success(),
        "stdout={}\nstderr={}",
        post.stdout,
        post.stderr
    );
    assert!(post.stdout.contains("COMPLETE WITH ERRORS"));
    assert!(!post.stdout.contains("LIVE / COPY-LARGE"));

    assert!(
        all.status.success(),
        "stdout={}\nstderr={}",
        all.stdout,
        all.stderr
    );
    assert!(all.stdout.contains("LIVE / COPY-LARGE"));
    assert!(all.stdout.contains("COMPLETE WITH ERRORS"));
}

#[test]
fn dry_run_reports_planned_copies_without_writing_files() {
    let root = TempDir::new("pathsync-dry-run");
    let source = root.path().join("source");
    let target = root.path().join("target");
    fs::create_dir_all(&source).unwrap();
    fs::create_dir_all(&target).unwrap();
    write_file(&source.join("photo.jpg"), b"dry-run-bytes");

    let config_path = write_config(&root, &source, &target, "size_mtime", "\"flat\"");
    let output = run_pathsync(&["--config", config_path.to_str().unwrap(), "--dry-run"]);

    assert!(
        output.status.success(),
        "stdout={}\nstderr={}",
        output.stdout,
        output.stderr
    );
    assert!(output.stdout.contains("dry run for job `sync`: 1 file(s)"));
    assert!(
        output
            .stdout
            .contains(&source.join("photo.jpg").display().to_string())
    );
    assert!(
        output
            .stdout
            .contains(&target.join("photo.jpg").display().to_string())
    );
    assert!(!target.join("photo.jpg").exists());
}

#[test]
fn real_copy_preserves_contents_and_mtime() {
    let root = TempDir::new("pathsync-real-copy");
    let source = root.path().join("source");
    let target = root.path().join("target");
    fs::create_dir_all(&source).unwrap();
    fs::create_dir_all(&target).unwrap();
    let source_file = source.join("photo.jpg");
    write_file(&source_file, b"real-copy-bytes");
    let mtime = FileTime::from_unix_time(1_700_000_000, 0);
    set_file_mtime(&source_file, mtime).unwrap();

    let config_path = write_config(&root, &source, &target, "size_mtime", "\"flat\"");
    let output = run_pathsync(&["--config", config_path.to_str().unwrap()]);

    assert!(
        output.status.success(),
        "stdout={}\nstderr={}",
        output.stdout,
        output.stderr
    );

    let copied = target.join("photo.jpg");
    assert_eq!(fs::read(&copied).unwrap(), b"real-copy-bytes");
    let copied_mtime = FileTime::from_last_modification_time(&fs::metadata(&copied).unwrap());
    assert_eq!(copied_mtime.unix_seconds(), mtime.unix_seconds());
    assert!(output.stdout.contains("SYNC COMPLETE"));
    assert!(output.stdout.contains("Result       success"));
    assert!(output.stdout.contains("\nSummary\n"));
    assert!(output.stdout.contains("\nCounts\n"));
    assert!(output.stdout.contains("\nCopied Files\n"));
    assert!(output.stdout.contains("photo.jpg"));
}

#[test]
fn rerun_under_size_mtime_skips_unchanged_files() {
    let root = TempDir::new("pathsync-size-mtime");
    let source = root.path().join("source");
    let target = root.path().join("target");
    fs::create_dir_all(&source).unwrap();
    fs::create_dir_all(&target).unwrap();
    let source_file = source.join("photo.jpg");
    write_file(&source_file, b"skip-me");
    let mtime = FileTime::from_unix_time(1_700_000_000, 0);
    set_file_mtime(&source_file, mtime).unwrap();

    let config_path = write_config(&root, &source, &target, "size_mtime", "\"flat\"");

    let first = run_pathsync(&["--config", config_path.to_str().unwrap()]);
    assert!(
        first.status.success(),
        "stdout={}\nstderr={}",
        first.stdout,
        first.stderr
    );

    let second = run_pathsync(&["--config", config_path.to_str().unwrap()]);
    assert!(
        second.status.success(),
        "stdout={}\nstderr={}",
        second.stdout,
        second.stderr
    );
    assert!(
        second
            .stdout
            .contains("no new files to copy for job `sync`")
    );

    let plans = build_transfer_plan(&load_job(&source, &target, "size_mtime"), false).unwrap();
    assert!(plans.is_empty());
}

#[test]
fn non_tty_copy_emits_plain_progress_lines_with_console_ui_contract() {
    let root = TempDir::new("pathsync-progress-plain");
    let source = root.path().join("source");
    let target = root.path().join("target");
    fs::create_dir_all(&source).unwrap();
    fs::create_dir_all(&target).unwrap();
    write_file(&source.join("one/photo.jpg"), &[b'a'; 256]);
    write_file(&source.join("two/photo.jpg"), &[b'b'; 256]);

    let config_path = root.path().join("config.toml");
    let text = format!(
        r#"
default_job = "sync"

[jobs.sync]
enabled = true
source = "{source}"
target = "{target}"
extensions = ["jpg"]
compare = {{ mode = "path" }}
transfer = {{ mode = "adaptive", large_file_threshold_mb = 1 }}
layout = {{ kind = "template", value = "{{source_rel_dir}}/{{filename}}" }}
parallel = 2
"#,
        source = source.display(),
        target = target.display(),
    );
    fs::write(&config_path, text).unwrap();

    let output = run_pathsync(&["--config", config_path.to_str().unwrap()]);

    assert!(
        output.status.success(),
        "stdout={}\nstderr={}",
        output.stdout,
        output.stderr
    );
    assert!(output.stdout.contains("phase    : adaptive"));
    assert!(output.stdout.contains("copying files |"));
    assert!(output.stdout.contains("copy complete |"));
    assert!(output.stdout.contains("W01"));
    assert!(output.stdout.contains("W02"));
    assert!(!output.stdout.contains("[W00]"));
    assert!(!output.stdout.contains("[W01]"));
    assert!(!output.stdout.contains("phase    : large files"));
    assert!(!output.stdout.contains("phase    : small files"));
    assert!(output.stdout.contains("one/photo.jpg"));
    assert!(output.stdout.contains("two/photo.jpg"));
    assert!(output.stdout.contains("SYNC COMPLETE"));
    assert!(
        output
            .stdout
            .contains("------------------------------------------------------------------------")
    );
    assert!(output.stdout.contains("#   File"));
}

#[test]
fn final_summary_caps_copied_file_list_for_large_runs() {
    let root = TempDir::new("pathsync-summary-cap");
    let source = root.path().join("source");
    let target = root.path().join("target");
    fs::create_dir_all(&source).unwrap();
    fs::create_dir_all(&target).unwrap();

    for index in 0..12 {
        write_file(
            &source.join(format!("batch/photo-{index:02}.jpg")),
            format!("bytes-{index}").as_bytes(),
        );
    }

    let config_path = root.path().join("config.toml");
    let text = format!(
        r#"
default_job = "sync"

[jobs.sync]
enabled = true
source = "{source}"
target = "{target}"
extensions = ["jpg"]
compare = {{ mode = "path" }}
transfer = {{ mode = "standard" }}
layout = {{ kind = "template", value = "{{source_rel_dir}}/{{filename}}" }}
parallel = 2
"#,
        source = source.display(),
        target = target.display(),
    );
    fs::write(&config_path, text).unwrap();

    let output = run_pathsync(&["--config", config_path.to_str().unwrap()]);

    assert!(
        output.status.success(),
        "stdout={}\nstderr={}",
        output.stdout,
        output.stderr
    );
    assert!(output.stdout.contains("Showing 8 of 12 copied files."));
    let copied_files_section = output
        .stdout
        .split("Copied Files")
        .last()
        .expect("copied files section missing");
    assert!(!copied_files_section.contains("photo-11.jpg"));
}

#[cfg(unix)]
#[test]
fn failure_path_reports_complete_with_errors_and_never_success_text() {
    let root = TempDir::new("pathsync-progress-failure");
    let source = root.path().join("source");
    let target = root.path().join("target");
    fs::create_dir_all(&source).unwrap();
    fs::create_dir_all(&target).unwrap();
    write_file(&source.join("photo.jpg"), b"should-fail");

    let target_permissions = fs::metadata(&target).unwrap().permissions();
    fs::set_permissions(&target, fs::Permissions::from_mode(0o555)).unwrap();

    let config_path = write_config(&root, &source, &target, "path", "\"flat\"");
    let output = run_pathsync(&["--config", config_path.to_str().unwrap()]);

    fs::set_permissions(&target, target_permissions).unwrap();

    assert!(
        !output.status.success(),
        "stdout={}\nstderr={}",
        output.stdout,
        output.stderr
    );
    assert!(!output.stdout.contains("all copies complete"));
    assert!(output.stdout.contains("ATTENTION ITEMS") || output.stderr.contains("ATTENTION ITEMS"));
    assert!(output.stdout.contains("copy failed") || output.stderr.contains("copy failed"));
    assert!(output.stdout.contains("\nFailures\n"));
}

#[cfg(unix)]
#[test]
fn local_failure_still_allows_later_phases_and_successful_copies() {
    let root = TempDir::new("pathsync-best-effort-local");
    let source = root.path().join("source");
    let target = root.path().join("target");
    let blocked = target.join("blocked");
    let open = target.join("open");
    fs::create_dir_all(&source).unwrap();
    fs::create_dir_all(&blocked).unwrap();
    fs::create_dir_all(&open).unwrap();

    write_file(&source.join("blocked/large.jpg"), &[b'x'; 2_000_000]);
    write_file(&source.join("open/small.jpg"), &[b'y'; 64]);

    let blocked_permissions = fs::metadata(&blocked).unwrap().permissions();
    fs::set_permissions(&blocked, fs::Permissions::from_mode(0o555)).unwrap();

    let config_path = root.path().join("config.toml");
    let text = format!(
        r#"
default_job = "sync"

[jobs.sync]
enabled = true
source = "{source}"
target = "{target}"
extensions = ["jpg"]
compare = {{ mode = "path" }}
transfer = {{ mode = "adaptive", large_file_threshold_mb = 1 }}
layout = {{ kind = "template", value = "{{source_rel_dir}}/{{filename}}" }}
parallel = 2
"#,
        source = source.display(),
        target = target.display(),
    );
    fs::write(&config_path, text).unwrap();

    let output = run_pathsync(&["--config", config_path.to_str().unwrap()]);

    fs::set_permissions(&blocked, blocked_permissions).unwrap();

    assert!(
        !output.status.success(),
        "stdout={}\nstderr={}",
        output.stdout,
        output.stderr
    );
    assert!(target.join("open/small.jpg").exists());
    assert!(!target.join("blocked/large.jpg").exists());
    assert!(output.stdout.contains("phase    : adaptive"));
    assert!(output.stdout.contains("copying files |"));
    assert!(output.stdout.contains("W01"));
    assert!(!output.stdout.contains("[W00]"));
    assert!(!output.stdout.contains("phase    : large files"));
    assert!(!output.stdout.contains("phase    : small files"));
    assert!(output.stdout.contains("[local]"));
    assert!(output.stdout.contains("ATTENTION ITEMS"));
    assert!(output.stdout.contains("Systemic"));
    assert!(output.stdout.contains("no"));
}

#[cfg(unix)]
#[test]
fn repeated_permission_failures_are_promoted_to_systemic() {
    let root = TempDir::new("pathsync-best-effort-systemic");
    let source = root.path().join("source");
    let target = root.path().join("target");
    let blocked = target.join("blocked");
    fs::create_dir_all(&source).unwrap();
    fs::create_dir_all(&blocked).unwrap();

    for index in 0..4 {
        write_file(
            &source.join(format!("blocked/photo-{index}.jpg")),
            format!("bytes-{index}").as_bytes(),
        );
    }

    let blocked_permissions = fs::metadata(&blocked).unwrap().permissions();
    fs::set_permissions(&blocked, fs::Permissions::from_mode(0o555)).unwrap();

    let config_path = root.path().join("config.toml");
    let text = format!(
        r#"
default_job = "sync"

[jobs.sync]
enabled = true
source = "{source}"
target = "{target}"
extensions = ["jpg"]
compare = {{ mode = "path" }}
transfer = {{ mode = "standard" }}
layout = {{ kind = "template", value = "{{source_rel_dir}}/{{filename}}" }}
parallel = 2
"#,
        source = source.display(),
        target = target.display(),
    );
    fs::write(&config_path, text).unwrap();

    let output = run_pathsync(&["--config", config_path.to_str().unwrap()]);

    fs::set_permissions(&blocked, blocked_permissions).unwrap();

    assert!(
        !output.status.success(),
        "stdout={}\nstderr={}",
        output.stdout,
        output.stderr
    );
    assert!(output.stdout.contains("[local]"));
    assert!(output.stdout.contains("[systemic]"));
    assert!(output.stdout.contains("Systemic"));
    assert!(output.stdout.contains("yes"));
}

#[test]
fn planning_failure_precedes_copy_on_collisions() {
    let root = TempDir::new("pathsync-collision");
    let source = root.path().join("source");
    let target = root.path().join("target");
    fs::create_dir_all(&source).unwrap();
    fs::create_dir_all(&target).unwrap();
    write_file(&source.join("one/photo.jpg"), b"1111");
    write_file(&source.join("two/photo.jpg"), b"2222");

    let config_path = write_config(
        &root,
        &source,
        &target,
        "size_mtime",
        "{ kind = \"template\", value = \"{filename}\" }",
    );
    let config = config::load_config(&config_path).unwrap();
    let job = config::resolve_job(&config, None, None, false, None).unwrap();
    let err = build_transfer_plan(&job, false).unwrap_err();
    assert!(matches!(
        err,
        PathsyncError::Plan(pathsync::plan::PlanError::Collision { .. })
    ));
    assert!(!target.join("photo.jpg").exists());
}

fn load_job(source: &Path, target: &Path, compare_mode: &str) -> config::ResolvedJob {
    let config = config::Config {
        default_job: Some("sync".to_string()),
        parallel: None,
        timezone: None,
        jobs: [(
            "sync".to_string(),
            config::JobConfig {
                enabled: Some(true),
                source: source.to_path_buf(),
                target: target.to_path_buf(),
                extensions: vec!["jpg".to_string()],
                compare: Some(config::CompareConfig {
                    mode: Some(compare_mode.to_string()),
                }),
                transfer: Some(config::TransferConfig {
                    mode: Some("standard".to_string()),
                    large_file_threshold_mb: None,
                    large_file_slots: None,
                }),
                parallel: None,
                timezone: None,
                layout: config::LayoutConfig::Preset("flat".to_string()),
            },
        )]
        .into_iter()
        .collect(),
    };

    config::resolve_job(&config, None, None, false, None).unwrap()
}
