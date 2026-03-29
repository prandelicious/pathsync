# Adaptive Weighted Concurrency Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Replace the current phase-split `adaptive` transfer mode with a user-tuned weighted concurrency policy that uses the job's `parallel` value as a slot budget.

**Architecture:** Extend `TransferPolicy::Adaptive` to include both a large-file threshold and a large-file slot cost. Replace the current `large files` then `small files` execution model with a single adaptive scheduler that assigns worker IDs dynamically, launches copies when their slot cost fits the remaining budget, and backfills smaller files when a large file would exceed the current free slots. Keep `standard` mode unchanged.

**Tech Stack:** Rust, `crossbeam-channel`, `indicatif`, existing integration/unit test suites.

---

### Task 1: Plan and config surface

**Files:**
- Modify: `src/policy.rs`
- Modify: `src/config.rs`
- Modify: `src/error.rs`
- Test: `tests/config_date.rs`
- Test: `tests/public_api.rs`

**Step 1: Write the failing tests**

- Add a config test proving `transfer = { mode = "adaptive", large_file_threshold_mb = 100 }` resolves to an adaptive policy whose large-file slot cost defaults to the resolved `parallel` value.
- Add a config test proving `large_file_slots = 2` is preserved.
- Add a config test proving `large_file_slots = 0` is rejected with a typed config error.
- Update the public API exposure test to assert the expanded `TransferPolicy::Adaptive` shape.

**Step 2: Run tests to verify they fail**

Run: `cargo test --test config_date --test public_api`

Expected: failures on adaptive policy shape / missing `large_file_slots` handling.

**Step 3: Write minimal implementation**

- Add `large_file_slots: Option<usize>` to `TransferConfig`.
- Extend `TransferPolicy::Adaptive` with `large_file_slots: usize`.
- Change `resolve_transfer_policy` to accept the resolved `parallel` value so adaptive defaults can use the job slot budget.
- Reject zero-valued adaptive slot cost with a dedicated config error.

**Step 4: Run tests to verify they pass**

Run: `cargo test --test config_date --test public_api`

Expected: PASS.

### Task 2: Adaptive scheduler behavior

**Files:**
- Modify: `src/copy.rs`
- Test: `tests/copy_integration.rs`

**Step 1: Write the failing tests**

- Add an integration test showing adaptive mode no longer emits separate `large files` / `small files` phase headers.
- Add an integration test proving a large file can run alongside a small file when `parallel` exceeds `large_file_slots`.
- Preserve the existing best-effort failure behavior by asserting a blocked large-file copy still allows smaller work that fits remaining slots to complete.

**Step 2: Run tests to verify they fail**

Run: `cargo test --test copy_integration`

Expected: failures because adaptive is still phase-based.

**Step 3: Write minimal implementation**

- Replace phase-split adaptive execution with a mixed queue scheduler.
- Treat `parallel` as total slot budget.
- Treat small files as cost `1`; large files as cost `large_file_slots`.
- Sort pending adaptive work by size descending and backfill with the next fitting item when the largest pending item does not fit current free slots.
- Assign worker IDs from an idle pool so UI bars still map to active copies cleanly.
- Keep `standard` mode on the current fixed-parallel path.
- Decouple large/small summary accounting from execution phase so adaptive still reports bucket counts even though it runs as one mixed phase.

**Step 4: Run tests to verify they pass**

Run: `cargo test --test copy_integration`

Expected: PASS.

### Task 3: Progress, dry-run, and docs

**Files:**
- Modify: `src/copy.rs`
- Modify: `src/progress_model.rs`
- Modify: `README.md`

**Step 1: Write the failing tests**

- Update any progress/output tests that currently assume adaptive emits `large files` / `small files` phases.
- Add or update a unit test covering the adaptive dry-run / summary wording if needed.

**Step 2: Run tests to verify they fail**

Run: `cargo test --test progress_format --test progress_model --test copy_integration`

Expected: failures from outdated wording assumptions.

**Step 3: Write minimal implementation**

- Adjust adaptive progress labeling so it uses a single mixed/adaptive phase label.
- Keep `standard` progress output behavior intact.
- Update README transfer mode docs and config example to document slot-budget semantics and `large_file_slots`.

**Step 4: Run tests to verify they pass**

Run: `cargo test --test progress_format --test progress_model --test copy_integration`

Expected: PASS.

### Task 4: Final verification

**Files:**
- Modify: none
- Test: whole repo

**Step 1: Run formatting**

Run: `cargo fmt --check`

Expected: PASS.

**Step 2: Run full verification**

Run: `cargo test`

Expected: PASS.
