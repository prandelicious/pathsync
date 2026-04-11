# Dual-Target Copy Design Notes

**Status:** Exploratory design notes

**Goal:** Document how `pathsync` could support copying one source job to two or more destination roots in a single run.

## Current State

Today each job resolves to exactly one target directory.

- `/Users/francis/Developer/projects/pathsync/src/config.rs` defines `JobConfig.target: PathBuf`.
- `/Users/francis/Developer/projects/pathsync/src/config.rs` defines `ResolvedJob.target: PathBuf`.
- `/Users/francis/Developer/projects/pathsync/src/plan.rs` defines `PlanJob.target: PathBuf`.
- `/Users/francis/Developer/projects/pathsync/src/plan.rs` renders one final destination path per source file.

This means the current implementation is single-target only.

## Approaches Considered

### 1. Duplicate jobs in config

Define two jobs with the same source and policy settings but different `target` values.

Example:

```toml
[jobs.vlog_ssd]
source = "/Volumes/Camera/DCIM"
target = "/Volumes/SSD/Vlog"
extensions = ["mp4", "jpg"]
layout = "year_month"

[jobs.vlog_backup]
source = "/Volumes/Camera/DCIM"
target = "/Volumes/Archive/Vlog"
extensions = ["mp4", "jpg"]
layout = "year_month"
```

Pros:
- Almost no code change.
- Reuses the current planning and copy pipeline unchanged.
- Easy to explain.

Cons:
- Scans the source tree twice.
- Plans the same work twice.
- Produces separate runs and separate summaries.
- Makes it harder to treat the copies as one logical sync operation.

### 2. Add `targets = []` to a job and expand the plan per target

Allow one job to declare multiple destination roots, then create one transfer plan entry per rendered destination.

Example:

```toml
[jobs.vlog]
source = "/Volumes/Camera/DCIM"
targets = [
  "/Volumes/SSD/Vlog",
  "/Volumes/Archive/Vlog",
]
extensions = ["mp4", "jpg"]
layout = "year_month"
```

Pros:
- Scans the source tree once.
- Keeps one logical job, one run, and one summary.
- Reuses most of the existing copy pipeline if `TransferPlan` remains one source/one destination.
- Existing worker parallelism can naturally interleave work across both targets.

Cons:
- Requires config, planning, and tests to change.
- Failure reporting becomes more nuanced because one target may succeed while another fails.
- Collision handling must still work per fully rendered destination path.

### 3. Keep one logical source plan and fan out during execution

Build a source-relative plan once, then replicate each transfer to multiple targets only at execution time.

Pros:
- Can model a source file once and fan it out later.
- Could make per-target retry and reporting cleaner in the long term.

Cons:
- Larger refactor.
- Less aligned with the current data model, where `TransferPlan` already stores one resolved `dest` path.
- Adds complexity before there is evidence that the extra abstraction is needed.

## Recommended Direction

Recommend approach 2: support `targets = []` at the job level and expand to destination-specific `TransferPlan` values during planning.

Why this is the best fit now:
- It preserves the current mental model of a transfer as one source copied to one destination path.
- It avoids rescanning the source tree for each output root.
- It keeps the implementation smaller than a full plan/execution split.
- It leaves room for later refactoring if richer per-target behavior is needed.

## Proposed Config Shape

Prefer backward compatibility.

- Keep `target` for existing configs.
- Add optional `targets` for multi-target jobs.
- Reject configs that specify both `target` and `targets`.
- Normalize both forms into `Vec<PathBuf>` during resolution.

Possible config behavior:

- `target = "/path"` becomes `targets = ["/path"]` internally.
- `targets = []` is invalid.
- Every resolved target must already exist as a directory, matching today's validation rule.

## Proposed Code Changes

### `/Users/francis/Developer/projects/pathsync/src/config.rs`

- Change the job config model to accept either `target` or `targets`.
- Validate that exactly one form is provided.
- Resolve the selected form into `ResolvedJob.targets: Vec<PathBuf>`.
- Validate each target directory.

### `/Users/francis/Developer/projects/pathsync/src/lib.rs`

- Change planning setup to pass all resolved targets into `PlanJob`.
- Update job-printing output so multi-target jobs list every destination.

### `/Users/francis/Developer/projects/pathsync/src/plan.rs`

- Change `PlanJob.target` to `PlanJob.targets: Vec<PathBuf>`.
- Render the relative destination once per source file.
- For each target root, create one `TransferPlan` with a fully resolved destination path.
- Keep collision detection keyed by final destination path.

### `/Users/francis/Developer/projects/pathsync/src/copy.rs`

- Keep the current execution shape if each `TransferPlan` still maps to one destination.
- Let existing parallel scheduling process transfers to either target.
- Improve summary output so users can tell which target failed when failures are partial.

### Tests

Add or update tests to cover:
- config parsing for `target`
- config parsing for `targets`
- rejection when both are present
- rejection when neither is present
- planning expansion from one source file to multiple destinations
- collision handling across fully rendered destination paths
- partial failure reporting when one target succeeds and another fails

## Decided failure semantics

This behavior is now decided.

If a file copies successfully to target A but fails for target B:

1. the run continues copying remaining target-specific transfers
2. the post-run report records the failing destination path
3. the CLI reports `COMPLETE WITH ERRORS`
4. the overall run still exits successfully for multi-target jobs when the failures are target-local rather than systemic

Planning/config failures and systemic runtime failures remain fatal.

## Minimal First Version

The implemented first version:
- adds `targets = []` with backward-compatible `target`
- expands plans to one `TransferPlan` per target
- reuses the current execution scheduling
- returns success for multi-target runs that complete with only target-local failures
- still returns a non-zero exit code for planning/config failures and systemic runtime failures

That version provides multi-target copying without redesigning the execution engine.

## Summary

If `pathsync` needs to copy to two locations at once, the most practical design is:

- one job
- one source scan
- multiple resolved target roots
- one transfer record per final destination path
- one combined run with clearer per-target reporting

This keeps the current architecture mostly intact while adding the behavior users expect from a multi-destination sync job.
