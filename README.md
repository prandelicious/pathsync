# pathsync

`pathsync` is a config-driven file sync tool that scans source trees, renders destination paths from a template, and copies files into a target directory.

## Config

Configuration is TOML. The top-level keys are:

- `default_job`: optional job name to use when no job is passed on the CLI
- `parallel`: optional default worker count for jobs that do not set their own
- `timezone`: optional default timezone for filesystem-mtime date extraction
- `jobs`: map of job names to job definitions

Each job supports:

- `enabled`: optional boolean, defaults to `true`
- `source`: source directory to scan
- `target`: destination directory root
- `extensions`: list of allowed file extensions, without leading dots
- `compare`: optional compare policy
- `transfer`: optional transfer policy
- `parallel`: optional per-job worker count
- `timezone`: optional per-job timezone override for filesystem-mtime date extraction
- `layout`: destination layout preset or template

## Compare Modes

`compare.mode` accepts:

- `path`
- `path_size`
- `size_mtime`

If `compare.mode` is omitted, `size_mtime` is the default.

- `path` only checks whether the destination path already exists
- `path_size` checks destination existence plus file size
- `size_mtime` checks file size and modified time

## Transfer Modes

`transfer.mode` accepts:

- `standard`
- `adaptive`

If `transfer.mode` is omitted, `standard` is the default.

- `standard` uses the resolved `parallel` value as a fixed worker count for all files.
- `adaptive` uses the resolved `parallel` value as a slot budget.

Adaptive transfer options:

- `large_file_threshold_mb`: files at or above this size are treated as large. Defaults to `100`.
- `large_file_slots`: slot cost for each large file. Defaults to the resolved `parallel` value.

Adaptive scheduling rules:

- Small files consume `1` slot.
- Large files consume `large_file_slots`.
- `pathsync` prefers larger files first and backfills smaller files when they fit the remaining slot budget.
- Setting `large_file_slots = parallel` preserves the original behavior of running large files one at a time.

## Layouts

`layout` may be:

- `"flat"`: place files directly under the target root
- `"year_month"`: group by extracted year and month
- `{ kind = "template", value = "..." }`: use an explicit template

Available template tokens:

- `{year}`
- `{month}`
- `{day}`
- `{ext}`
- `{stem}`
- `{filename}`
- `{source_rel_dir}`

## Date Extraction

`pathsync` derives `{year}`, `{month}`, and `{day}` like this:

- JPEG/TIFF images: EXIF capture date first, then filesystem modified time if EXIF is missing or unreadable
- Other files including videos: filesystem modified time

Timezone rules:

- Filesystem mtimes use the resolved timezone policy for the job.
- Timezone precedence is job `timezone`, then top-level `timezone`, then implicit `"local"`.
- Supported values are `"local"`, `"UTC"`, and IANA names like `"America/Los_Angeles"`.
- EXIF-derived dates are treated as literal captured calendar values and are not reinterpreted through the configured timezone.

Filename patterns are not used for date extraction.

## Safety Rules

- Rendered destination paths must remain relative to the job target.
- Absolute rendered paths are rejected.
- Escaping paths such as `..` segments are rejected.
- Unresolved template tokens are rejected.
- If two source files resolve to the same destination, planning fails instead of silently picking one.

## Copy Failure Reporting

- Planning and config failures still abort before copying starts.
- Runtime copy failures are best-effort: `pathsync` continues through the rest of the planned files and reports all failures at the end.
- The final summary marks each runtime failure as `[local]` or `[systemic]`.
- `Systemic` becomes `yes` when a job-wide failure pattern is detected.
- Permission failures start as local and are promoted to systemic after 3 prior permission failures in the same job.

## Copy Performance

`pathsync` now picks OS-native full-file copy paths automatically when the platform supports them. Production config and CLI behavior are unchanged: there is no new transfer mode, flag, or tuning knob for this.

When a native path is unavailable or cannot be used safely, `pathsync` falls back to the existing manual copy loop.

## Benchmarking

The `bench-copy` binary compares copy strategies on the same source and target storage.

- `--method all` runs `native`, `buffered`, and `stdio`, and is the default.
- `--method native` measures the production native copy path directly.
- `--method both` remains backward-compatible and means `buffered` plus `stdio`.
- `--method buffered` and `--method stdio` still measure the existing non-native paths individually.

## Example

```toml
default_job = "vlog"
parallel = 4
timezone = "UTC"

[jobs.vlog]
enabled = true
source = "/Volumes/Go-Ultra/DCIM/Camera01"
target = "/Volumes/T7/Videos/Vlog"
extensions = ["mp4", "jpg"]
compare = { mode = "size_mtime" }
transfer = { mode = "adaptive", large_file_threshold_mb = 100 }
timezone = "America/Los_Angeles"
layout = "year_month"
```

Example with backfill-friendly adaptive settings:

```toml
default_job = "vlog"
parallel = 4

[jobs.vlog]
enabled = true
source = "/Volumes/Go-Ultra/DCIM/Camera01"
target = "/Volumes/T7/Videos/Vlog"
extensions = ["mp4", "jpg"]
compare = { mode = "size_mtime" }
transfer = { mode = "adaptive", large_file_threshold_mb = 100, large_file_slots = 3 }
layout = "year_month"
```
