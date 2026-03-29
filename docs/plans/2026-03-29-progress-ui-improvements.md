# Progress UI Improvements Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Improve copy progress output so it is truthful on failure, clearer during multi-phase runs, easier to read with duplicate filenames, and usable in non-interactive terminals.

**Architecture:** Split progress behavior into two new pure helper modules so parallel workers can implement and test them without colliding in `src/copy.rs`. Keep `src/copy.rs` as the event/orchestration layer that consumes those helpers and owns `indicatif` integration.

**Tech Stack:** Rust, `indicatif`, existing integration tests, `cargo test`, `cargo clippy`, `cargo fmt`

---

**Execution model:** one coordinator plus parallel worker sub-agents. Every implementation worker uses `agent_type=worker`, `model=gpt-5.4-mini`, `reasoning_effort=medium`. Do not let two workers edit the same file.

### Task 1: Extract Progress State Model

**Files:**
- Create: `src/progress_model.rs`
- Modify: `src/lib.rs`
- Test: `tests/progress_model.rs`

**Owner:** Worker A

**Step 1: Write the failing tests**

Add pure-model tests for:
- outcome label returns `all copies complete` only when no worker error occurred
- failure outcome returns `copy failed` or `completed with errors`
- phase label distinguishes `large files` and `small files`
- ETA returns `None` when rate is zero and a positive duration otherwise
- worker count for the active phase is `min(phase_task_count, configured_parallel)` with a minimum of `1` when tasks exist

**Step 2: Run tests to verify they fail**

Run: `CARGO_HOME=/tmp/pathsync-cargo cargo test --test progress_model`

Expected: FAIL because `src/progress_model.rs` does not exist yet.

**Step 3: Write minimal implementation**

Create `src/progress_model.rs` with:
- `PhaseKind` enum: `LargeFiles`, `SmallFiles`
- `Outcome` enum: `Success`, `Failure`
- `ProgressSnapshot` struct holding completed/task counts, active workers, bytes done/total, elapsed, phase, and failure flag
- pure helpers:
  - `overall_message(&ProgressSnapshot) -> String`
  - `phase_label(PhaseKind) -> &'static str`
  - `eta(bytes_done, bytes_total, elapsed) -> Option<Duration>`
  - `active_worker_slots(configured_parallel, phase_task_count) -> usize`

Export the new module from `src/lib.rs`.

**Step 4: Run tests to verify they pass**

Run: `CARGO_HOME=/tmp/pathsync-cargo cargo test --test progress_model`

Expected: PASS

**Step 5: Commit**

```bash
git add src/progress_model.rs src/lib.rs tests/progress_model.rs
git commit -m "feat(progress): add progress state model"
```

### Task 2: Extract Progress Formatting Helpers

**Files:**
- Create: `src/progress_format.rs`
- Modify: `src/lib.rs`
- Test: `tests/progress_format.rs`

**Owner:** Worker B

**Step 1: Write the failing tests**

Add pure-format tests for:
- duplicate filenames produce disambiguated worker labels using truncated relative path + filename
- overall line includes phase label and ETA when available
- plain-text fallback line is stable and single-line
- worker idle line does not render empty numeric fields

**Step 2: Run tests to verify they fail**

Run: `CARGO_HOME=/tmp/pathsync-cargo cargo test --test progress_format`

Expected: FAIL because `src/progress_format.rs` does not exist yet.

**Step 3: Write minimal implementation**

Create `src/progress_format.rs` with pure helpers:
- `worker_label(display_name: &str, source: &Path, root: &Path, max_chars: usize) -> String`
- `overall_line(snapshot: &ProgressSnapshot) -> String`
- `worker_line(label: &str, bytes: u64, elapsed: Duration) -> String`
- `plain_progress_line(snapshot: &ProgressSnapshot) -> String`

Rules:
- prefer `source` relative path when it disambiguates duplicate basenames
- include ETA only when non-zero rate is available
- preserve the existing byte/rate formatting style unless changed by a test

Export the new module from `src/lib.rs`.

**Step 4: Run tests to verify they pass**

Run: `CARGO_HOME=/tmp/pathsync-cargo cargo test --test progress_format`

Expected: PASS

**Step 5: Commit**

```bash
git add src/progress_format.rs src/lib.rs tests/progress_format.rs
git commit -m "feat(progress): add progress formatting helpers"
```

### Task 3: Wire Model + Formatting into Copy Execution

**Files:**
- Modify: `src/copy.rs`
- Modify: `src/lib.rs`
- Test: `tests/copy_integration.rs`

**Owner:** Coordinator

**Step 1: Write the failing tests**

Extend `tests/copy_integration.rs` with coverage for:
- failure path does not print `all copies complete`
- CLI progress text shows current phase label for large/small phases
- duplicate source basenames produce distinguishable worker labels in progress text
- non-interactive execution emits plain progress lines instead of `indicatif` multi-bar output

Use a deliberately failing source/target case for the failure assertion and run the binary with captured stdout/stderr so the behavior is asserted at the user boundary.

**Step 2: Run tests to verify they fail**

Run: `CARGO_HOME=/tmp/pathsync-cargo cargo test --test copy_integration`

Expected: FAIL on the new progress assertions.

**Step 3: Write minimal implementation**

Modify `src/copy.rs` to:
- import and use `progress_model` and `progress_format`
- track the current phase explicitly in `run_copy`
- allocate worker bars per phase using `active_worker_slots` instead of always using `job.parallel`
- replace `display_name`-only worker messages with disambiguated labels derived from source path
- remove duplicate error printing by making the progress UI the single source of per-file error text
- set the final overall message from `Outcome` so failure never prints `all copies complete`
- add non-interactive fallback:
  - if stdout is not a terminal, do not build `MultiProgress`
  - print a single line on phase start and periodic plain-text updates using `plain_progress_line`

Keep the existing copy semantics unchanged except for user-facing progress output.

**Step 4: Run tests to verify they pass**

Run: `CARGO_HOME=/tmp/pathsync-cargo cargo test --test copy_integration`

Expected: PASS

**Step 5: Commit**

```bash
git add src/copy.rs src/lib.rs tests/copy_integration.rs
git commit -m "feat(progress): improve copy progress output"
```

### Task 4: Final Verification and Cleanup

**Files:**
- Modify only if needed: `README.md`

**Owner:** Worker C or Coordinator

**Step 1: Write the failing doc/check test**

If Task 3 changes any user-visible progress wording documented in `README.md`, add the minimal doc update and, if helpful, a smoke assertion in `tests/copy_integration.rs` for the final wording.

**Step 2: Run verification**

Run:
- `cargo fmt --check`
- `CARGO_HOME=/tmp/pathsync-cargo cargo test`
- `CARGO_HOME=/tmp/pathsync-cargo cargo clippy --all-targets --all-features -- -D warnings`

Expected: all commands PASS

**Step 3: Commit**

```bash
git add README.md src/copy.rs src/lib.rs tests/progress_model.rs tests/progress_format.rs tests/copy_integration.rs
git commit -m "chore(progress): finalize progress ui improvements"
```

## Parallel Dispatch Notes

- Batch 1 can run in parallel:
  - Worker A owns `src/progress_model.rs` and `tests/progress_model.rs`
  - Worker B owns `src/progress_format.rs` and `tests/progress_format.rs`
- Batch 2 is coordinator-only because `src/copy.rs` is the shared integration point.
- Batch 3 can run only after Task 3 if a docs touch is needed.

## Reviewer Checklist

- No failure path prints `all copies complete`
- Progress output clearly states large-file vs small-file phase
- Duplicate basenames are distinguishable in worker output
- Non-TTY output is readable and non-animated
- Error lines are emitted once per failure
- Full repo verification is green
