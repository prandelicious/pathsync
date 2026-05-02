# pathsync

`pathsync` is a Rust CLI for config-driven file sync jobs. It scans a source tree, renders each destination path from a layout, skips files that already match the selected compare policy, and copies the remaining files into one or more target directories.

The repository currently ships two binaries:

- `pathsync`: the sync CLI
- `bench-copy`: a helper for benchmarking copy backends on real storage

## Current capabilities

- Multiple named jobs in one TOML config
- CLI overrides for job name, config path, parallelism, extensions, and disabled jobs
- Dry-run planning without writing files
- Backward-compatible `target` plus first-class multi-target `targets = [..]` job configuration
- Flat, year/month, and fully templated destination layouts
- Date extraction from EXIF for JPEG/TIFF, with filesystem mtime fallback
- Timezone-aware filesystem date extraction
- `path`, `path_size`, and `size_mtime` compare policies
- `standard` and adaptive transfer scheduling
- Best-effort runtime copy handling with end-of-run failure summaries
- Streaming destination verification by target using runtime `xxh3_128` file signatures
- Collision protection for rendered destination paths
  - different source files that render to the same final destination fail planning
- Preview screens for the live and post-copy terminal UI
- Native-copy benchmarking through `bench-copy`

## Build and install

Install a current Rust toolchain first.

### Using `just`

This repo defines the common workflows in `Justfile`:

```bash
just build
just install
just clean
```

- `just build` builds `target/release/pathsync`
- `just install` copies that binary to `~/.local/bin/pathsync`
- `just clean` removes build artifacts

### Using Cargo directly

```bash
cargo build --release
cargo install --path . --bins
```

After installation:

```bash
pathsync --help
bench-copy --help
```

## CLI usage

Run the default job from the default config location:

```bash
pathsync
```

Run a specific job from an explicit config file:

```bash
pathsync --config /path/to/config.toml JOB_NAME
```

List configured jobs without validating source or target directories:

```bash
pathsync --config examples/config.toml --list-jobs
```

Preview the terminal UI without loading config:

```bash
pathsync --preview-ui all
```

Useful flags:

- `--config <PATH>`: read a specific TOML config
- `--list-jobs`: print configured jobs and exit
- `--dry-run`: print planned copies without writing files
- `--force`: copy files even when the compare policy would skip them
- `--parallel <N>`: override the resolved worker count
- `--extensions <csv>`: override the configured extension allow-list
- `--allow-disabled`: run a job even if `enabled = false`
- `--preview-ui <live|post-copy|all>`: render canned UI previews and exit

If `--config` is omitted, `pathsync` reads:

- `$XDG_CONFIG_HOME/pathsync/config.toml`, when `XDG_CONFIG_HOME` is set
- otherwise `~/.config/pathsync/config.toml`

If no job name is passed, `pathsync` uses `default_job` when present. Otherwise it picks the first enabled job. If `default_job` points at a disabled job, it falls back to the first enabled job.

## Configuration

Configuration is TOML with these top-level keys:

- `default_job`: optional default job name
- `parallel`: optional default worker count
- `timezone`: optional default timezone for filesystem-mtime date extraction
- `jobs`: map of job names to job definitions

Each job supports:

- `enabled`: optional boolean, defaults to `true`
- `source`: source directory to scan
- `target`: one destination directory root
- `targets`: multiple destination directory roots
- `extensions`: allowed file extensions
- `compare`: optional compare policy
- `transfer`: optional transfer policy
- `parallel`: optional per-job worker count override
- `timezone`: optional per-job timezone override
- `layout`: destination layout preset or template

Notes:

- configure either `target` or `targets`, never both
- `target = "/path"` is normalized internally to `targets = ["/path"]`
- `targets = []` is invalid
- `source` and every configured target must already exist as directories
- extensions are normalized by trimming whitespace, removing any leading `.`, and lowercasing
- `parallel` defaults to `4` when neither the CLI nor config sets it
- `parallel = 0` is rejected

## Compare policies

`compare.mode` accepts:

- `path`
- `path_size`
- `size_mtime` (default)

Behavior:

- `path`: skip when the rendered destination path already exists
- `path_size`: skip when the destination exists and the file size matches
- `size_mtime`: skip when file size and modified time match

## Transfer policies

`transfer.mode` accepts:

- `standard` (default)
- `adaptive`

### `standard`

Uses the resolved `parallel` value as a fixed worker count.

### `adaptive`

Uses the resolved `parallel` value as a slot budget.

Options:

- `large_file_threshold_mb`: files at or above this size are large, default `100`
- `large_file_slots`: slot cost for each large file, default `parallel`
- `max_large_per_target`: maximum concurrent large-file copies per target, default `min(2, parallel)`

Rules:

- small files consume `1` slot
- large files consume `large_file_slots`
- larger files are scheduled first, then smaller files backfill remaining slots
- in multi-target jobs, large transfers for the same source share one slot cost so target fan-out can run in parallel
- faster target lanes can continue with later large files while slower target lanes finish earlier work
- `large_file_slots` must be between `1` and `parallel`
- `max_large_per_target` must be between `1` and `parallel`
- `large_file_slots = parallel` allows one distinct large source at a time

## Layouts and template tokens

`layout` may be:

- `"flat"`: place files directly under each target root
- `"year_month"`: place files under `{year}/{month}/` beneath each target root
- `{ kind = "flat" }`
- `{ kind = "year_month" }`
- `{ kind = "template", value = "..." }`

Available template tokens:

- `{year}`
- `{month}`
- `{day}`
- `{ext}`
- `{stem}`
- `{filename}`
- `{source_rel_dir}`

Template safety rules:

- rendered paths must stay relative to the configured target root
- absolute paths are rejected
- escaping paths such as `..` are rejected
- unresolved template tokens are rejected

## Date extraction and timezones

`pathsync` derives `{year}`, `{month}`, and `{day}` like this:

- JPEG and TIFF: EXIF capture date first, then filesystem modified time if EXIF is missing or unreadable
- all other files, including videos: filesystem modified time

Timezone rules:

- filesystem mtimes use the resolved timezone policy
- precedence is job `timezone`, then top-level `timezone`, then implicit `local`
- supported values are `local`, `UTC`, and IANA names such as `America/Los_Angeles`
- EXIF-derived dates are treated as literal captured calendar values and are not reinterpreted through the configured timezone

Filename-based date parsing is not used.

## Planning and copy behavior

Before copying, `pathsync` builds a plan from the source tree.

- files outside the extension allow-list are ignored
- the source tree is scanned once per job, even when the job has multiple targets
- each rendered relative destination expands to one planned transfer per target root
- if no target-specific transfers need copying, the CLI prints `no new files to copy for job ...`
- a dry run prints one source → destination mapping per planned transfer
- compare-policy skip checks run per fully resolved destination path
- if two different source files render to the same final destination:
  - planning aborts with a collision error
  - planning does not read full file contents to dedupe collisions

During copying:

- `pathsync` preserves file modification times on copied files
- runtime copy failures are best-effort; later files still run when possible
- for multi-target jobs, target-local copy failures do not stop other target copies from running
- copy workers compute a `(size, xxh3_128)` source signature immediately before copying and check that the source metadata is unchanged after copying
- same-source multi-target transfers reuse the runtime source-signature cache
- successful copies are streamed to bounded verifier workers, which read destinations only and compare them with the source signature
- live progress and final completion are based on verified bytes/transfers
- the post-run summary includes `Target Results` with planned, copied, verified, copy-failed, and verify-failed counts per target
- any verification miss or signature mismatch returns a failed run result
- skipped-existing files are not verified unless copied in the current run
- single-target runtime failures still return a failed run result
- planning and config failures still stop the run before copying starts
- final summaries classify failures as `[local]` or `[systemic]`
- post-run error rows include destination-path detail so you can see which target failed
- repeated permission failures can be promoted from local to systemic

For terminal output:

- TTY runs render the full-screen live/post-copy UI
- non-TTY runs emit plain progress lines and a text summary

## Copy backend and benchmarking

Production `pathsync` automatically uses OS-native full-file copy paths when they are available and safe. There is no extra transfer mode or CLI flag for this behavior.

When a native path is unavailable, `pathsync` falls back to the existing manual copy loop.

Use `bench-copy` to compare copy methods on the same source and target storage:

```bash
bench-copy --source /path/to/file --target-dir /path/to/dir --runs 3 --method all
```

Supported benchmark methods:

- `native`
- `buffered`
- `stdio`
- `both` (`buffered` + `stdio`)
- `all` (`native` + `buffered` + `stdio`, default)

## Example config

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
transfer = { mode = "adaptive", large_file_threshold_mb = 100, large_file_slots = 3 }
timezone = "America/Los_Angeles"
layout = "year_month"

[jobs.backup_vlog]
enabled = true
source = "/Volumes/Go-Ultra/DCIM/Camera01"
targets = [
  "/Volumes/T7/Videos/Vlog",
  "/Volumes/Archive/Videos/Vlog",
]
extensions = ["mp4", "jpg"]
compare = { mode = "size_mtime" }
transfer = { mode = "adaptive", large_file_threshold_mb = 100, large_file_slots = 3 }
timezone = "America/Los_Angeles"
layout = "year_month"
```

Template-based layout example:

```toml
[jobs.template_example]
enabled = false
source = "/path/to/source"
target = "/path/to/target"
extensions = ["mp4", "jpg"]
compare = { mode = "size_mtime" }
transfer = { mode = "adaptive", large_file_threshold_mb = 250 }
layout = { kind = "template", value = "{year}/{month}/{ext}/{source_rel_dir}/{filename}" }
```

See `/Users/francis/Developer/projects/pathsync/examples/config.toml` for a checked-in sample config.
