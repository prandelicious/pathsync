use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::SystemTime;

use filetime::{FileTime, set_file_mtime};
use pathsync::config::{ResolvedJob, ResolvedStaging};
use pathsync::policy::{ComparePolicy, TimezonePolicy, TransferPolicy};
use pathsync::{build_transfer_plan, build_transfer_plan_with_stats, config};
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

fn write_multi_target_config(
    root: &TempDir,
    source: &Path,
    targets: &[&Path],
    compare_mode: &str,
    layout: &str,
) -> PathBuf {
    let config_path = root.path().join("config.toml");
    let targets = targets
        .iter()
        .map(|target| format!("\"{}\"", target.display()))
        .collect::<Vec<_>>()
        .join(", ");
    let text = format!(
        r#"
default_job = "sync"

[jobs.sync]
enabled = true
source = "{source}"
targets = [{targets}]
extensions = ["jpg"]
compare = {{ mode = "{compare_mode}" }}
transfer = {{ mode = "standard" }}
layout = {layout}
"#,
        source = source.display(),
        targets = targets,
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

fn run_bench_copy(args: &[&str]) -> CommandOutput {
    let output = Command::new(env!("CARGO_BIN_EXE_bench-copy"))
        .args(args)
        .output()
        .expect("failed to run bench-copy");
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
    assert!(!live.stdout.contains("ATTENTION"));

    assert!(
        post.status.success(),
        "stdout={}\nstderr={}",
        post.stdout,
        post.stderr
    );
    assert!(post.stdout.contains("ATTENTION"));
    assert!(!post.stdout.contains("LIVE / COPY-LARGE"));

    assert!(
        all.status.success(),
        "stdout={}\nstderr={}",
        all.stdout,
        all.stderr
    );
    assert!(all.stdout.contains("LIVE / COPY-LARGE"));
    assert!(all.stdout.contains("ATTENTION"));
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
fn dry_run_reports_each_multi_target_mapping_without_writing_files() {
    let root = TempDir::new("pathsync-dry-run-multi-target");
    let source = root.path().join("source");
    let target_a = root.path().join("target-a");
    let target_b = root.path().join("target-b");
    fs::create_dir_all(&source).unwrap();
    fs::create_dir_all(&target_a).unwrap();
    fs::create_dir_all(&target_b).unwrap();
    write_file(&source.join("photo.jpg"), b"dry-run-bytes");

    let config_path = write_multi_target_config(
        &root,
        &source,
        &[&target_a, &target_b],
        "size_mtime",
        "\"flat\"",
    );
    let output = run_pathsync(&["--config", config_path.to_str().unwrap(), "--dry-run"]);

    assert!(
        output.status.success(),
        "stdout={}\nstderr={}",
        output.stdout,
        output.stderr
    );
    assert!(output.stdout.contains("dry run for job `sync`: 2 file(s)"));
    assert!(
        output
            .stdout
            .contains(&target_a.join("photo.jpg").display().to_string())
    );
    assert!(
        output
            .stdout
            .contains(&target_b.join("photo.jpg").display().to_string())
    );
    assert!(!target_a.join("photo.jpg").exists());
    assert!(!target_b.join("photo.jpg").exists());
}

#[test]
fn list_jobs_reports_all_multi_target_roots() {
    let root = TempDir::new("pathsync-list-jobs-multi-target");
    let source = root.path().join("source");
    let target_a = root.path().join("target-a");
    let target_b = root.path().join("target-b");
    fs::create_dir_all(&source).unwrap();
    fs::create_dir_all(&target_a).unwrap();
    fs::create_dir_all(&target_b).unwrap();

    let config_path = write_multi_target_config(
        &root,
        &source,
        &[&target_a, &target_b],
        "size_mtime",
        "\"flat\"",
    );
    let output = run_pathsync(&["--config", config_path.to_str().unwrap(), "--list-jobs"]);

    assert!(
        output.status.success(),
        "stdout={}\nstderr={}",
        output.stdout,
        output.stderr
    );
    assert!(output.stdout.contains("targets    :"));
    assert!(output.stdout.contains(&target_a.display().to_string()));
    assert!(output.stdout.contains(&target_b.display().to_string()));
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
    assert!(output.stdout.contains("VERIFIED"));
    assert!(output.stdout.contains("Result       VERIFIED"));
    assert!(output.stdout.contains("\nRun\n"));
    assert!(output.stdout.contains("\nBreakdown\n"));
    assert!(output.stdout.contains("\nTarget Results\n"));
    assert!(output.stdout.contains("Verified"));
    assert!(output.stdout.contains("\nCopied file preview\n"));
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
    assert!(output.stdout.contains("T01"));
    assert!(output.stdout.contains("T02"));
    assert!(!output.stdout.contains("[W00]"));
    assert!(!output.stdout.contains("[W01]"));
    assert!(!output.stdout.contains("phase    : large files"));
    assert!(!output.stdout.contains("phase    : small files"));
    assert!(output.stdout.contains("one/photo.jpg"));
    assert!(output.stdout.contains("two/photo.jpg"));
    assert!(output.stdout.contains("VERIFIED"));
    assert!(output.stdout.contains("target "));
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
    assert!(output.stdout.contains("showing 8 of 12 copied files"));
    let copied_files_section = output
        .stdout
        .split("Copied file preview")
        .last()
        .expect("copied files section missing");
    assert!(!copied_files_section.contains("photo-11.jpg"));
}

#[test]
fn bench_copy_defaults_to_all_and_includes_native_method() {
    let root = TempDir::new("pathsync-bench-all");
    let source_dir = root.path().join("source");
    let target = root.path().join("target");
    fs::create_dir_all(&source_dir).unwrap();
    fs::create_dir_all(&target).unwrap();
    write_file(&source_dir.join("photo.jpg"), b"bench-all-bytes");

    let output = run_bench_copy(&[
        "--source",
        source_dir.join("photo.jpg").to_str().unwrap(),
        "--target-dir",
        target.to_str().unwrap(),
        "--runs",
        "1",
    ]);

    assert!(
        output.status.success(),
        "stdout={}\nstderr={}",
        output.stdout,
        output.stderr
    );
    assert!(output.stdout.contains("method: native"));
    assert!(output.stdout.contains("method: buffered"));
    assert!(output.stdout.contains("method: stdio"));
    assert!(output.stdout.contains("comparison:"));

    let _ = fs::remove_dir_all(root.path());
}

#[test]
fn bench_copy_accepts_explicit_native_method() {
    let root = TempDir::new("pathsync-bench-native");
    let source_dir = root.path().join("source");
    let target = root.path().join("target");
    fs::create_dir_all(&source_dir).unwrap();
    fs::create_dir_all(&target).unwrap();
    write_file(&source_dir.join("photo.jpg"), b"bench-native-bytes");

    let output = run_bench_copy(&[
        "--source",
        source_dir.join("photo.jpg").to_str().unwrap(),
        "--target-dir",
        target.to_str().unwrap(),
        "--runs",
        "1",
        "--method",
        "native",
    ]);

    assert!(
        output.status.success(),
        "stdout={}\nstderr={}",
        output.stdout,
        output.stderr
    );
    assert!(output.stdout.contains("method: native"));
    assert!(!output.stdout.contains("method: buffered"));
    assert!(!output.stdout.contains("comparison:"));

    let _ = fs::remove_dir_all(root.path());
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
    assert!(output.stdout.contains("ATTENTION") || output.stderr.contains("ATTENTION"));
    assert!(output.stdout.contains("copy failed") || output.stderr.contains("copy failed"));
    assert!(output.stdout.contains("\nFailures\n"));
}

#[cfg(unix)]
#[test]
fn multi_target_local_failure_reports_failed_target_without_end_verification() {
    let root = TempDir::new("pathsync-multi-target-local-failure");
    let source = root.path().join("source");
    let blocked = root.path().join("blocked-target");
    let open = root.path().join("open-target");
    fs::create_dir_all(&source).unwrap();
    fs::create_dir_all(&blocked).unwrap();
    fs::create_dir_all(&open).unwrap();
    write_file(&source.join("photo.jpg"), b"multi-target-bytes");

    let blocked_permissions = fs::metadata(&blocked).unwrap().permissions();
    fs::set_permissions(&blocked, fs::Permissions::from_mode(0o555)).unwrap();

    let config_path =
        write_multi_target_config(&root, &source, &[&blocked, &open], "path", "\"flat\"");
    let output = run_pathsync(&["--config", config_path.to_str().unwrap()]);

    fs::set_permissions(&blocked, blocked_permissions).unwrap();

    assert!(
        !output.status.success(),
        "stdout={}\nstderr={}",
        output.stdout,
        output.stderr
    );
    assert!(!blocked.join("photo.jpg").exists());
    assert!(open.join("photo.jpg").exists());
    assert!(output.stdout.contains("ATTENTION"));
    assert!(output.stdout.contains("CopyFail"));
    assert!(!output.stdout.contains("\nVerification\n"));
    assert!(output.stdout.contains("photo.jpg"));
    assert!(
        output
            .stdout
            .contains(&blocked.join("photo.jpg").display().to_string())
    );
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
    assert!(output.stdout.contains("T01"));
    assert!(!output.stdout.contains("[W00]"));
    assert!(!output.stdout.contains("phase    : large files"));
    assert!(!output.stdout.contains("phase    : small files"));
    assert!(output.stdout.contains("\nFailures\n"));
    assert!(output.stdout.contains("ATTENTION"));
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
    assert!(output.stdout.contains("\nFailures\n"));
    assert!(output.stdout.contains("FAILED"));
    assert!(output.stdout.contains("Systemic"));
    assert!(output.stdout.contains("yes"));
}

#[cfg(unix)]
#[test]
fn failed_rename_cleans_up_temp_file() {
    let root = TempDir::new("pathsync-temp-cleanup");
    let source = root.path().join("source");
    let target = root.path().join("target");
    fs::create_dir_all(&source).unwrap();
    fs::create_dir_all(&target).unwrap();
    write_file(&source.join("photo.jpg"), b"cleanup-bytes");

    let final_dest = target.join("photo.jpg");
    fs::create_dir_all(&final_dest).unwrap();
    let temp_dest = target.join("photo.jpg.pathsync-part");

    let config_path = write_config(&root, &source, &target, "path", "\"flat\"");
    let output = run_pathsync(&["--config", config_path.to_str().unwrap(), "--force"]);

    assert!(
        !output.status.success(),
        "stdout={}\nstderr={}",
        output.stdout,
        output.stderr
    );
    assert!(
        !temp_dest.exists(),
        "temp file was left behind: {}",
        temp_dest.display()
    );
    assert!(final_dest.is_dir());
    assert!(output.stdout.contains("copy failed") || output.stderr.contains("copy failed"));
}

#[test]
fn planning_rejects_same_destination_collisions_without_content_dedupe() {
    let root = TempDir::new("pathsync-collision");
    let source = root.path().join("source");
    let target = root.path().join("target");
    fs::create_dir_all(&source).unwrap();
    fs::create_dir_all(&target).unwrap();
    write_file(&source.join("one/photo.jpg"), b"1111");
    write_file(&source.join("two/photo.jpg"), b"1111");

    let config_path = write_config(
        &root,
        &source,
        &target,
        "size_mtime",
        "{ kind = \"template\", value = \"{filename}\" }",
    );
    let config = config::load_config(&config_path).unwrap();
    let job = config::resolve_job(&config, None, None, false, None).unwrap();

    let error = build_transfer_plan(&job, false).unwrap_err();
    match error {
        pathsync::error::PathsyncError::Plan(pathsync::plan::PlanError::Collision {
            destination,
            sources,
        }) => {
            assert_eq!(destination, target.join("photo.jpg"));
            assert_eq!(sources.len(), 2);
        }
        other => panic!("expected collision error, got {other:?}"),
    }
}

/// Directly constructs a staged `ResolvedJob`, bypassing config parsing.
/// `ResolvedStaging.max_bytes` is byte-granular, while the TOML `staging`
/// config table only accepts whole `max_gb` (and rejects `0`), so tests
/// that need a small/precise cap (e.g. "cap smaller than the largest
/// planned file") build a `ResolvedJob` directly here rather than writing
/// a config file, the same way `load_job` below already does for
/// non-staged jobs.
fn resolved_staged_job(
    source: &Path,
    targets: Vec<PathBuf>,
    transfer_policy: TransferPolicy,
    staging: ResolvedStaging,
) -> ResolvedJob {
    ResolvedJob {
        name: "sync".to_string(),
        source: source.to_path_buf(),
        targets,
        extensions: vec!["jpg".to_string()],
        compare_policy: ComparePolicy::SizeMtime,
        transfer_policy,
        timezone_policy: TimezonePolicy::Local,
        parallel: 4,
        template: "{filename}".to_string(),
        staging: Some(staging),
    }
}

#[test]
fn staged_run_with_cap_smaller_than_largest_file_fails_fast_without_partial_writes() {
    let root = TempDir::new("pathsync-staging-cap-too-small");
    let source = root.path().join("source");
    let target = root.path().join("target");
    let spool_dir = root.path().join("spool");
    fs::create_dir_all(&source).unwrap();
    fs::create_dir_all(&target).unwrap();
    fs::create_dir_all(&spool_dir).unwrap();
    write_file(&source.join("big.jpg"), &[b'x'; 4096]);

    let job = resolved_staged_job(
        &source,
        vec![target.clone()],
        TransferPolicy::Standard,
        ResolvedStaging {
            dir: spool_dir.clone(),
            max_bytes: Some(1024),
            min_free_bytes: 0,
        },
    );

    let plan_build = build_transfer_plan_with_stats(&job, false).unwrap();
    assert_eq!(plan_build.plans.len(), 1);

    let error = pathsync::copy::run_copy(&job, plan_build.plans, plan_build.stats)
        .expect_err("run must fail fast when the cap is smaller than the largest planned file");
    match error {
        pathsync::error::CopyError::StagingValidationFailed { message } => {
            assert!(
                message.contains("capacity cap"),
                "unexpected message: {message}"
            );
        }
        other => panic!("expected StagingValidationFailed, got {other:?}"),
    }

    assert!(
        !target.join("big.jpg").exists(),
        "no partial target write must exist after fail-fast validation"
    );
    let spool_entries: Vec<_> = fs::read_dir(&spool_dir).unwrap().collect();
    assert!(
        spool_entries.is_empty(),
        "no spool residue after fail-fast validation: {spool_entries:?}"
    );
}

#[test]
fn staged_run_only_drains_to_targets_that_still_need_the_file() {
    let root = TempDir::new("pathsync-staging-compare-interplay");
    let source = root.path().join("source");
    let target_a = root.path().join("target-a");
    let target_b = root.path().join("target-b");
    let spool_dir = root.path().join("spool");
    fs::create_dir_all(&source).unwrap();
    fs::create_dir_all(&target_a).unwrap();
    fs::create_dir_all(&target_b).unwrap();
    fs::create_dir_all(&spool_dir).unwrap();

    let source_file = source.join("photo.jpg");
    write_file(&source_file, b"shared-bytes");
    let mtime = FileTime::from_unix_time(1_700_000_000, 0);
    set_file_mtime(&source_file, mtime).unwrap();

    // Pre-seed target A with an already-matching copy (same size + mtime)
    // so planning produces no `TransferPlan` for A, only for B -- existing
    // planning behavior (untouched by staging).
    let existing = target_a.join("photo.jpg");
    write_file(&existing, b"shared-bytes");
    set_file_mtime(&existing, mtime).unwrap();

    let job = resolved_staged_job(
        &source,
        vec![target_a.clone(), target_b.clone()],
        TransferPolicy::Standard,
        ResolvedStaging {
            dir: spool_dir.clone(),
            max_bytes: None,
            min_free_bytes: 0,
        },
    );

    let plan_build = build_transfer_plan_with_stats(&job, false).unwrap();
    assert_eq!(
        plan_build.plans.len(),
        1,
        "planning must skip the already-matching target A copy"
    );
    assert_eq!(plan_build.plans[0].dest, target_b.join("photo.jpg"));

    let result = pathsync::copy::run_copy(&job, plan_build.plans, plan_build.stats);
    assert!(result.is_ok(), "{result:?}");

    assert_eq!(
        fs::read(target_b.join("photo.jpg")).unwrap(),
        b"shared-bytes"
    );
    assert_eq!(
        fs::read(&existing).unwrap(),
        b"shared-bytes",
        "target A's pre-existing file must be untouched"
    );

    let spool_entries: Vec<_> = fs::read_dir(&spool_dir).unwrap().collect();
    assert!(
        spool_entries.is_empty(),
        "spool run directory must be cleaned up once draining finishes: {spool_entries:?}"
    );
}

#[test]
fn staged_single_target_job_completes_and_reports_source_released() {
    let root = TempDir::new("pathsync-staging-single-target");
    let source = root.path().join("source");
    let target = root.path().join("target");
    let spool_dir = root.path().join("spool");
    fs::create_dir_all(&source).unwrap();
    fs::create_dir_all(&target).unwrap();
    write_file(&source.join("photo.jpg"), b"staged-single-target-bytes");

    let config_path = root.path().join("config.toml");
    let text = format!(
        r#"
default_job = "sync"

[jobs.sync]
enabled = true
source = "{source}"
target = "{target}"
extensions = ["jpg"]
compare = {{ mode = "size_mtime" }}
transfer = {{ mode = "standard" }}
layout = "flat"

[jobs.sync.staging]
dir = "{staging_dir}"
max_gb = 1
min_free_gb = 0
"#,
        source = source.display(),
        target = target.display(),
        staging_dir = spool_dir.display(),
    );
    fs::write(&config_path, text).unwrap();

    let output = run_pathsync(&["--config", config_path.to_str().unwrap()]);

    assert!(
        output.status.success(),
        "stdout={}\nstderr={}",
        output.stdout,
        output.stderr
    );
    assert_eq!(
        fs::read(target.join("photo.jpg")).unwrap(),
        b"staged-single-target-bytes"
    );
    assert!(
        output.stdout.contains("source released"),
        "stdout={}",
        output.stdout
    );
    assert!(output.stdout.contains("VERIFIED"));
}

#[test]
fn staged_adaptive_run_stages_and_verifies_mixed_large_and_small_files() {
    let root = TempDir::new("pathsync-staging-adaptive");
    let source = root.path().join("source");
    let target = root.path().join("target");
    let spool_dir = root.path().join("spool");
    fs::create_dir_all(&source).unwrap();
    fs::create_dir_all(&target).unwrap();
    write_file(&source.join("large.jpg"), &[b'L'; 2_000_000]);
    write_file(&source.join("small.jpg"), &[b'S'; 64]);

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
layout = "flat"
parallel = 2

[jobs.sync.staging]
dir = "{staging_dir}"
max_gb = 1
min_free_gb = 0
"#,
        source = source.display(),
        target = target.display(),
        staging_dir = spool_dir.display(),
    );
    fs::write(&config_path, text).unwrap();

    let output = run_pathsync(&["--config", config_path.to_str().unwrap()]);

    assert!(
        output.status.success(),
        "stdout={}\nstderr={}",
        output.stdout,
        output.stderr
    );
    assert_eq!(fs::read(target.join("large.jpg")).unwrap().len(), 2_000_000);
    assert_eq!(fs::read(target.join("small.jpg")).unwrap(), &[b'S'; 64][..]);
    assert!(output.stdout.contains("source released"));
    assert!(output.stdout.contains("VERIFIED"));
}

#[cfg(unix)]
#[test]
fn staged_run_with_unreadable_source_reports_failure_and_still_releases() {
    let root = TempDir::new("pathsync-staging-failure");
    let source = root.path().join("source");
    let target = root.path().join("target");
    let spool_dir = root.path().join("spool");
    fs::create_dir_all(&source).unwrap();
    fs::create_dir_all(&target).unwrap();
    write_file(&source.join("good.jpg"), b"good-bytes");
    let bad = source.join("bad.jpg");
    write_file(&bad, b"bad-bytes");
    fs::set_permissions(&bad, fs::Permissions::from_mode(0o000)).unwrap();

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
layout = "flat"

[jobs.sync.staging]
dir = "{staging_dir}"
max_gb = 1
min_free_gb = 0
"#,
        source = source.display(),
        target = target.display(),
        staging_dir = spool_dir.display(),
    );
    fs::write(&config_path, text).unwrap();

    let output = run_pathsync(&["--config", config_path.to_str().unwrap()]);

    fs::set_permissions(&bad, fs::Permissions::from_mode(0o644)).unwrap();

    assert!(
        !output.status.success(),
        "stdout={}\nstderr={}",
        output.stdout,
        output.stderr
    );
    assert!(
        output.stdout.contains("ATTENTION"),
        "stdout={}",
        output.stdout
    );
    assert!(
        output.stdout.contains("source released"),
        "release milestone must still fire once every planned file is terminal: stdout={}",
        output.stdout
    );
    assert!(
        output.stdout.contains("with staging failures"),
        "release event must note the staging failure: stdout={}",
        output.stdout
    );
    assert_eq!(fs::read(target.join("good.jpg")).unwrap(), b"good-bytes");
    assert!(!target.join("bad.jpg").exists());
}

fn load_job(source: &Path, target: &Path, compare_mode: &str) -> config::ResolvedJob {
    let config = config::Config {
        default_job: Some("sync".to_string()),
        parallel: None,
        timezone: None,
        staging: None,
        jobs: [(
            "sync".to_string(),
            config::JobConfig {
                enabled: Some(true),
                source: source.to_path_buf(),
                target: Some(target.to_path_buf()),
                targets: None,
                extensions: vec!["jpg".to_string()],
                compare: Some(config::CompareConfig {
                    mode: Some(compare_mode.to_string()),
                }),
                transfer: Some(config::TransferConfig {
                    mode: Some("standard".to_string()),
                    large_file_threshold_mb: None,
                    large_file_slots: None,
                    max_large_per_target: None,
                }),
                parallel: None,
                timezone: None,
                staging: None,
                layout: config::LayoutConfig::Preset("flat".to_string()),
            },
        )]
        .into_iter()
        .collect(),
    };

    config::resolve_job(&config, None, None, false, None).unwrap()
}
