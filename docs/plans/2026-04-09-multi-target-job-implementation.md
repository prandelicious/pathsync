# Multi-Target Job Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Add first-class support for `targets = [..]` so one `pathsync` job can copy the same planned files to two or more destination roots in a single run.

**Architecture:** Keep the current execution model in which each `TransferPlan` represents one source file copied to one fully resolved destination path. Extend config resolution to normalize `target` and `targets` into `ResolvedJob.targets: Vec<PathBuf>`, then expand each planned source file into one transfer per target root during planning. Preserve best-effort runtime copying, but for multi-target jobs do not fail the whole run when a subset of target copies fails; instead, continue copying remaining transfers and surface target-specific failures in the post-copy report.

**Tech Stack:** Rust, `serde`, `toml`, existing planning/copy/progress modules, unit tests in `tests/*.rs`.

---

## Accepted product decisions

These decisions are part of this plan and should not be reopened during implementation unless new constraints appear.

- Users configure one destination with `target = "/path"` or many destinations with `targets = ["/a", "/b"]`.
- `target` remains supported for backward compatibility.
- `target` and `targets` are mutually exclusive.
- `targets = []` is invalid.
- Every target directory must already exist.
- Planning scans the source tree once and expands each rendered relative destination to one final destination per target root.
- Runtime copy remains best-effort.
- If a transfer fails for one target but succeeds for another, the job continues and the post-copy report shows the target-specific failure.
- Partial multi-target copy failures are reportable but not fatal to the overall run result. Planning/config errors and internal/systemic failures remain fatal.

## Codebase context

Implementation will touch these areas:

- `/Users/francis/Developer/projects/pathsync/src/config.rs`
  - config parsing and job resolution
- `/Users/francis/Developer/projects/pathsync/src/error.rs`
  - config error surface
- `/Users/francis/Developer/projects/pathsync/src/lib.rs`
  - run orchestration and plan construction
- `/Users/francis/Developer/projects/pathsync/src/plan.rs`
  - plan expansion and collision handling
- `/Users/francis/Developer/projects/pathsync/src/copy.rs`
  - copy execution result semantics and reporting context
- `/Users/francis/Developer/projects/pathsync/src/progress_model.rs`
  - post-run model data if target-aware error reporting needs new fields
- `/Users/francis/Developer/projects/pathsync/src/progress_format.rs`
  - report rendering if target-aware wording changes are needed
- `/Users/francis/Developer/projects/pathsync/README.md`
  - config and behavior docs
- `/Users/francis/Developer/projects/pathsync/examples/config.toml`
  - example config if the repo keeps examples current
- `/Users/francis/Developer/projects/pathsync/tests/config_date.rs`
  - config parsing and resolution tests
- `/Users/francis/Developer/projects/pathsync/tests/plan_layout.rs`
  - plan expansion and collision tests
- `/Users/francis/Developer/projects/pathsync/tests/copy_integration.rs`
  - end-to-end copy behavior
- `/Users/francis/Developer/projects/pathsync/tests/public_api.rs`
  - public API shape and behavior

## Implementation strategy

Follow TDD in small steps.

1. Extend the config surface and resolve jobs into a normalized multi-target shape.
2. Expand planning to produce one destination-specific transfer per target root.
3. Update runtime reporting and success semantics for partial multi-target failures.
4. Update docs and examples.
5. Run focused verification first, then repo-wide verification.

---

### Task 1: Add config tests for `target` and `targets`

**Files:**
- Modify: `/Users/francis/Developer/projects/pathsync/tests/config_date.rs`
- Test: `/Users/francis/Developer/projects/pathsync/tests/config_date.rs`

**Step 1: Write the failing tests**

Add tests that prove:

- a config with `target = "/path"` resolves to `ResolvedJob.targets == vec![path]`
- a config with `targets = ["/a", "/b"]` resolves to both paths in order
- specifying both `target` and `targets` returns a typed config error
- specifying neither `target` nor `targets` returns a typed config error
- specifying `targets = []` returns a typed config error
- a missing directory in `targets` returns a typed config error that names the missing path

Use temp directories for real path validation so resolution exercises current directory checks.

**Step 2: Run tests to verify they fail**

Run:

```bash
cargo test --test config_date
```

Expected: FAIL because `JobConfig` and `ResolvedJob` still model only one target and no typed validation exists for the new config shape.

**Step 3: Write minimal implementation**

In `/Users/francis/Developer/projects/pathsync/src/config.rs` and `/Users/francis/Developer/projects/pathsync/src/error.rs`:

- change `JobConfig` to:

```rust
pub struct JobConfig {
    pub enabled: Option<bool>,
    pub source: PathBuf,
    pub target: Option<PathBuf>,
    pub targets: Option<Vec<PathBuf>>,
    pub extensions: Vec<String>,
    pub compare: Option<CompareConfig>,
    pub transfer: Option<TransferConfig>,
    pub parallel: Option<usize>,
    pub timezone: Option<String>,
    pub layout: LayoutConfig,
}
```

- change `ResolvedJob` to:

```rust
pub struct ResolvedJob {
    pub name: String,
    pub source: PathBuf,
    pub targets: Vec<PathBuf>,
    pub extensions: Vec<String>,
    pub compare_policy: ComparePolicy,
    pub transfer_policy: TransferPolicy,
    pub timezone_policy: TimezonePolicy,
    pub parallel: usize,
    pub template: String,
}
```

- add a helper such as:

```rust
fn resolve_targets(job: &JobConfig) -> Result<Vec<PathBuf>, ConfigError>
```

- add typed config errors for:
  - both `target` and `targets` set
  - neither `target` nor `targets` set
  - empty `targets`
- keep `TargetFolderNotFound { path }` for missing directories so existing error style remains consistent
- validate every resolved target directory

**Step 4: Run tests to verify they pass**

Run:

```bash
cargo test --test config_date
```

Expected: PASS.

**Step 5: Commit**

```bash
git add tests/config_date.rs src/config.rs src/error.rs
git commit -m "feat: add multi-target job config resolution"
```

---

### Task 2: Update public API tests for normalized multi-target jobs

**Files:**
- Modify: `/Users/francis/Developer/projects/pathsync/tests/public_api.rs`
- Test: `/Users/francis/Developer/projects/pathsync/tests/public_api.rs`

**Step 1: Write the failing tests**

Update public API coverage to assert:

- `ResolvedJob.targets` is publicly accessible
- a single-target config still resolves through the compatibility path
- a multi-target config preserves both targets
- plan building returns one destination per target for the same source file

Add a concrete assertion like:

```rust
assert_eq!(job.targets, vec![target_a.clone(), target_b.clone()]);
assert_eq!(plans.len(), 2);
assert!(plans.iter().any(|plan| plan.dest == target_a.join("photo.jpg")));
assert!(plans.iter().any(|plan| plan.dest == target_b.join("photo.jpg")));
```

**Step 2: Run tests to verify they fail**

Run:

```bash
cargo test --test public_api
```

Expected: FAIL because public structs and plan behavior still assume one target.

**Step 3: Write minimal implementation**

Update the tested public structs and any compilation fallout caused by replacing `.target` with `.targets`.

**Step 4: Run tests to verify they pass**

Run:

```bash
cargo test --test public_api
```

Expected: PASS.

**Step 5: Commit**

```bash
git add tests/public_api.rs src/config.rs src/lib.rs src/plan.rs
git commit -m "refactor: expose normalized job targets in public api"
```

---

### Task 3: Add planning tests for plan expansion across targets

**Files:**
- Modify: `/Users/francis/Developer/projects/pathsync/tests/plan_layout.rs`
- Test: `/Users/francis/Developer/projects/pathsync/tests/plan_layout.rs`

**Step 1: Write the failing tests**

Add tests that prove:

- one source file under a multi-target job yields one `TransferPlan` per target root
- templated layouts render once and then fan out to every target root
- planning stats count the expanded transfer set correctly for `planned_files` and `planned_bytes`
- compare-policy skipping is evaluated per final destination path, not once globally
- collision handling still works per fully resolved destination path

Add one explicit test where target A already has the destination file and target B does not; expected outcome: one skipped transfer and one planned transfer.

**Step 2: Run tests to verify they fail**

Run:

```bash
cargo test --test plan_layout
```

Expected: FAIL because `PlanJob` still stores one target and planning does not fan out.

**Step 3: Write minimal implementation**

In `/Users/francis/Developer/projects/pathsync/src/plan.rs`:

- change `PlanJob` to:

```rust
pub struct PlanJob {
    pub source: PathBuf,
    pub targets: Vec<PathBuf>,
    pub extensions: Vec<String>,
    pub compare_policy: ComparePolicy,
    pub template: String,
}
```

- in `build_plan`, after `rel_dest` is rendered once, expand to one `dest` per target:

```rust
for target in &job.targets {
    let dest = target.join(&rel_dest);
    let plan = TransferPlan {
        source: source.clone(),
        dest: dest.clone(),
        size: metadata.len(),
        display_name: file_name.clone(),
    };
    candidates.entry(dest).or_default().push(plan);
}
```

- preserve one source-tree scan and one file-context computation per source file
- keep collision detection keyed by final absolute destination path
- keep skip checks per final `plan.dest`
- update plan sorting expectations if the expanded destination set changes order

**Step 4: Run tests to verify they pass**

Run:

```bash
cargo test --test plan_layout
```

Expected: PASS.

**Step 5: Commit**

```bash
git add tests/plan_layout.rs src/plan.rs src/lib.rs
git commit -m "feat: expand transfer plans across multiple targets"
```

---

### Task 4: Make run orchestration and dry-run output target-aware

**Files:**
- Modify: `/Users/francis/Developer/projects/pathsync/src/lib.rs`
- Modify: `/Users/francis/Developer/projects/pathsync/src/copy.rs`
- Test: `/Users/francis/Developer/projects/pathsync/tests/public_api.rs`
- Test: `/Users/francis/Developer/projects/pathsync/tests/copy_integration.rs`

**Step 1: Write the failing tests**

Add coverage for:

- `print_dry_run` prints both destination mappings for one source file in a multi-target job
- `--list-jobs` or equivalent job summary output lists all target roots clearly
- run orchestration still prints `no new files to copy` only when every target-specific transfer is skipped

If there is no existing command-output harness for `print_jobs`, add the smallest direct unit test that exercises the formatting helper.

**Step 2: Run tests to verify they fail**

Run:

```bash
cargo test --test public_api --test copy_integration
```

Expected: FAIL because output helpers still refer to a singular target.

**Step 3: Write minimal implementation**

In `/Users/francis/Developer/projects/pathsync/src/lib.rs`:

- build `PlanJob` with `targets: job.targets.clone()`
- update `print_jobs` so output is unambiguous:
  - single target compatibility can still print one line
  - multi-target jobs should print a `targets` section with one line per path

Suggested output shape:

```text
targets    : /Volumes/SSD/Vlog
             /Volumes/Archive/Vlog
```

In `/Users/francis/Developer/projects/pathsync/src/copy.rs`:

- keep `print_dry_run` as one line per transfer:

```text
/source/photo.jpg -> /target-a/photo.jpg
/source/photo.jpg -> /target-b/photo.jpg
```

- remove any remaining assumptions that a job has only one target in reporting context setup

**Step 4: Run tests to verify they pass**

Run:

```bash
cargo test --test public_api --test copy_integration
```

Expected: PASS.

**Step 5: Commit**

```bash
git add src/lib.rs src/copy.rs tests/public_api.rs tests/copy_integration.rs
git commit -m "refactor: make job output multi-target aware"
```

---

### Task 5: Add integration tests for partial multi-target copy failures

**Files:**
- Modify: `/Users/francis/Developer/projects/pathsync/tests/copy_integration.rs`
- Test: `/Users/francis/Developer/projects/pathsync/tests/copy_integration.rs`

**Step 1: Write the failing tests**

Add an integration test that creates:

- one source directory with at least one copyable file
- one writable target directory
- one target path that will fail at copy time in a controlled, portable way if possible

If a portable forced-write failure is not practical, split the test into:

- a lower-level execution/reporting test that injects a synthetic `CopyFailure`
- an integration test that proves successful target copies are still written when the plan includes more than one target

Required assertions:

- target A receives the copied file
- target B failure is recorded with the failing destination path
- the run continues through the remaining planned transfers
- the final result is not promoted to a fatal `CopyError::RunFailed` merely because one target failed
- the post-run data path marks the run as completed with errors rather than aborted

**Step 2: Run tests to verify they fail**

Run:

```bash
cargo test --test copy_integration
```

Expected: FAIL because current runtime copy failures end the run with `CopyError::RunFailed`.

**Step 3: Write minimal implementation**

In `/Users/francis/Developer/projects/pathsync/src/copy.rs`:

- separate `continue copying after a failure` from `return an error after the run`
- preserve best-effort execution during the run
- introduce a completion path that returns success when the run completed and all failures were target-local runtime failures
- keep fatal returns for internal/runtime invariants such as UI panic, worker panic, or explicitly systemic conditions if the existing code already distinguishes them
- make sure `CopyFailure.dest` is always populated for destination-specific failures so the report can identify which target failed

This likely means changing the final `CopyReport` evaluation from:

```rust
Err(CopyError::RunFailed { .. })
```

to a target-aware result rule closer to:

```rust
if report_has_only_nonfatal_copy_failures(&report) {
    Ok(())
} else {
    Err(...)
}
```

Do not broaden success so far that planning failures, internal panics, or configuration errors get swallowed.

**Step 4: Run tests to verify they pass**

Run:

```bash
cargo test --test copy_integration
```

Expected: PASS.

**Step 5: Commit**

```bash
git add tests/copy_integration.rs src/copy.rs
git commit -m "feat: report partial multi-target failures without failing the run"
```

---

### Task 6: Make the post-copy report clearly identify target-specific failures

**Files:**
- Modify: `/Users/francis/Developer/projects/pathsync/src/progress_model.rs`
- Modify: `/Users/francis/Developer/projects/pathsync/src/progress_format.rs`
- Modify: `/Users/francis/Developer/projects/pathsync/src/copy.rs`
- Test: `/Users/francis/Developer/projects/pathsync/tests/progress_model.rs`
- Test: `/Users/francis/Developer/projects/pathsync/tests/progress_format.rs`

**Step 1: Write the failing tests**

Add or update tests that prove:

- the post-run screen still renders when copy failures occurred but the run returned success
- error rows include enough destination detail to tell which target failed
- status wording reflects completed-with-errors rather than fatal-abort semantics

Prefer exact rendered-string assertions in formatter tests.

Example expectation pattern:

```rust
assert!(rendered.join("\n").contains("/Volumes/Archive/Vlog/photo.jpg"));
assert!(rendered.join("\n").contains("COMPLETE WITH ERRORS"));
```

**Step 2: Run tests to verify they fail**

Run:

```bash
cargo test --test progress_model --test progress_format
```

Expected: FAIL because existing report wording and/or preview rows do not guarantee target-aware error detail.

**Step 3: Write minimal implementation**

- Thread destination-path detail through the post-run error row model if it is not already preserved.
- Keep the top-line status as `COMPLETE WITH ERRORS` when nonfatal target failures occurred.
- Avoid changing the canonical layout more than necessary; prefer clearer content over a broader redesign.
- Ensure any truncated path formatting still keeps enough of the destination to disambiguate targets.

**Step 4: Run tests to verify they pass**

Run:

```bash
cargo test --test progress_model --test progress_format
```

Expected: PASS.

**Step 5: Commit**

```bash
git add src/progress_model.rs src/progress_format.rs src/copy.rs tests/progress_model.rs tests/progress_format.rs
git commit -m "feat: show target-specific failures in post-copy report"
```

---

### Task 7: Update README and example config

**Files:**
- Modify: `/Users/francis/Developer/projects/pathsync/README.md`
- Modify: `/Users/francis/Developer/projects/pathsync/examples/config.toml`
- Modify: `/Users/francis/Developer/projects/pathsync/docs/plans/2026-04-09-dual-target-copy-design.md`

**Step 1: Write the failing doc expectations**

Make a checklist of README sections that must change:

- top-level description of target directories
- config reference for `target` vs `targets`
- notes section describing validation rules
- planning/copy behavior section describing per-target expansion
- partial failure semantics for multi-target runs
- example config showing `targets = [..]`

**Step 2: Update documentation**

Document:

- backward compatibility for `target`
- preferred new multi-target syntax
- validation rules
- planning behavior
- dry-run behavior
- post-run partial failure reporting

Update the existing exploratory design note to record that failure semantics have now been decided.

**Step 3: Review docs for clarity**

Read the edited files back and remove vague wording like “handles multiple destinations” in favor of concrete statements such as “creates one planned transfer per target root.”

**Step 4: Commit**

```bash
git add README.md examples/config.toml docs/plans/2026-04-09-dual-target-copy-design.md
git commit -m "docs: describe multi-target job configuration"
```

---

### Task 8: Final verification

**Files:**
- Modify: none
- Test: whole repo

**Step 1: Run formatting**

Run:

```bash
cargo fmt --check
```

Expected: PASS.

**Step 2: Run linting**

Run:

```bash
cargo clippy --all-targets --all-features -- -D warnings
```

Expected: PASS.

**Step 3: Run focused tests again**

Run:

```bash
cargo test --test config_date --test plan_layout --test copy_integration --test progress_model --test progress_format --test public_api
```

Expected: PASS.

**Step 4: Run full test suite**

Run:

```bash
cargo test
```

Expected: PASS.

**Step 5: Sanity-check example behavior**

Create a temporary config with `targets = [..]` and run:

```bash
cargo run -- --config /tmp/pathsync-multi-target.toml --dry-run
```

Expected:
- one source scan
- one dry-run line per source/target pair
- no config error for valid `targets`

**Step 6: Record verification evidence**

Capture exactly which commands passed and any environment-specific limits.

---

## Notes for the implementing engineer

- Prefer adding `targets` without introducing a second planning abstraction. The existing `TransferPlan { source, dest, ... }` type already matches the expanded plan model.
- Do not re-scan the source tree per target.
- Keep ordering deterministic. If plan order changes, sort by fully resolved `dest` as today.
- Be careful with stats. `planned_files`, `planned_bytes`, `skipped_existing_files`, and `skipped_existing_bytes` should reflect expanded transfers, not only distinct source files.
- Preserve backward compatibility for existing single-target configs and tests wherever possible.
- Avoid broad semantic changes to single-target runtime failure handling unless the necessary code simplification clearly improves the design and all tests still express the intended behavior.
- If a portable filesystem failure is hard to simulate in integration tests, introduce the smallest seam needed for deterministic report testing rather than relying on platform-specific permissions behavior.
