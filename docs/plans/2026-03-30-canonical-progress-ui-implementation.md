# Canonical Progress UI Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Replace the current progress renderer with an exact 80-column live and post-run UI that matches the approved mockup.

**Architecture:** Preserve planning and runtime metrics in structured models, then render the TTY UI from canonical `LiveScreenModel` and `PostRunScreenModel` values. Keep `src/copy.rs` as the integration seam, but move layout ownership into pure 80-column formatting helpers.

**Tech Stack:** Rust, existing CLI architecture, pure formatter tests, integration tests, `cargo test`, `cargo clippy`, `cargo fmt`

---

### Task 1: Preserve Planning Metrics For The UI

**Files:**
- Modify: `src/plan.rs`
- Modify: `src/lib.rs`
- Test: `tests/plan_layout.rs`
- Test: `tests/public_api.rs`

**Step 1: Write the failing tests**

Add tests that require planning to preserve:
- scanned file count
- planned file count
- skipped-existing count
- collision or planning-failure visibility where applicable

**Step 2: Run tests to verify they fail**

Run:
- `cargo test --test plan_layout`
- `cargo test --test public_api`

Expected: FAIL because the current API only returns `Vec<TransferPlan>`.

**Step 3: Write minimal implementation**

Change the planning API to return a structured result that includes:
- `plans: Vec<TransferPlan>`
- `stats: PlanningStats`

Keep `build_transfer_plan` backward-compatible only if that does not hide required stats from the renderer. Prefer a new structured API if needed.

**Step 4: Run tests to verify they pass**

Run:
- `cargo test --test plan_layout`
- `cargo test --test public_api`

Expected: PASS

**Step 5: Commit**

```bash
git add src/plan.rs src/lib.rs tests/plan_layout.rs tests/public_api.rs
git commit -m "feat(progress): preserve planning stats for ui"
```

### Task 2: Define Canonical Screen Models

**Files:**
- Modify: `src/progress_model.rs`
- Test: `tests/progress_model.rs`

**Step 1: Write the failing tests**

Add tests for pure model builders that require:
- canonical live header text
- canonical post-run header text
- summary metric fields for live and post-run screens
- category rows for post-run output
- error preview rows
- stable worker row models including idle slots

**Step 2: Run tests to verify they fail**

Run: `cargo test --test progress_model`

Expected: FAIL because the current snapshot model lacks these structures.

**Step 3: Write minimal implementation**

Add canonical view-model types such as:
- `PlanningStats`
- `RuntimeStats`
- `LiveScreenModel`
- `PostRunScreenModel`
- `WorkerRowModel`
- `CategoryRowModel`
- `ErrorRowModel`

Build them from planning stats plus runtime state without embedding terminal formatting.

**Step 4: Run tests to verify they pass**

Run: `cargo test --test progress_model`

Expected: PASS

**Step 5: Commit**

```bash
git add src/progress_model.rs tests/progress_model.rs
git commit -m "feat(progress): add canonical screen models"
```

### Task 3: Add Exact 80-Column Formatter Snapshots

**Files:**
- Modify: `src/progress_format.rs`
- Test: `tests/progress_format.rs`

**Step 1: Write the failing tests**

Add exact-layout formatter tests for:
- live large-file screen at 80 columns
- live screen with idle workers
- post-run success screen
- post-run error screen

Each test should compare full rendered lines, not substring fragments.

**Step 2: Run tests to verify they fail**

Run: `cargo test --test progress_format`

Expected: FAIL because no canonical full-frame renderer exists.

**Step 3: Write minimal implementation**

Implement pure formatting helpers for:
- fixed-width title row
- exact divider rows
- stat-grid lines
- canonical progress bars
- worker rows
- category rows
- error rows
- full screen rendering at width 80

**Step 4: Run tests to verify they pass**

Run: `cargo test --test progress_format`

Expected: PASS

**Step 5: Commit**

```bash
git add src/progress_format.rs tests/progress_format.rs
git commit -m "feat(progress): add canonical 80-col renderer"
```

### Task 4: Replace The Live TTY Renderer

**Files:**
- Modify: `src/copy.rs`
- Test: `tests/copy_integration.rs`

**Step 1: Write the failing tests**

Extend integration coverage so live TTY behavior requires:
- canonical live header and divider structure
- canonical summary stats wording
- canonical overall progress row label
- stable worker slot count and idle rows
- phase-specific top-right status text

Use deterministic rendering tests where possible. Keep runtime assertions resilient to timing.

**Step 2: Run tests to verify they fail**

Run: `cargo test --test copy_integration`

Expected: FAIL because the current TTY branch still uses row-owned progress bars.

**Step 3: Write minimal implementation**

In `src/copy.rs`:
- replace the live TTY layout path with full-frame composition
- aggregate runtime stats required by the mockup
- redraw the whole 80-column frame from pure formatter output
- preserve plain output behavior separately

Do not change copy semantics.

**Step 4: Run tests to verify they pass**

Run: `cargo test --test copy_integration`

Expected: PASS for the new live-screen contract.

**Step 5: Commit**

```bash
git add src/copy.rs tests/copy_integration.rs
git commit -m "feat(progress): replace live tty renderer"
```

### Task 5: Replace The Post-Run Report

**Files:**
- Modify: `src/copy.rs`
- Test: `tests/copy_integration.rs`

**Step 1: Write the failing tests**

Add failing assertions for:
- canonical post-run title row
- canonical `Copy completion` row
- canonical `By Category` section
- canonical `Errors` section
- correct category aggregation and error preview ordering

**Step 2: Run tests to verify they fail**

Run: `cargo test --test copy_integration`

Expected: FAIL because the current report uses generic sections.

**Step 3: Write minimal implementation**

Replace the generic summary builder with canonical post-run screen rendering from `PostRunScreenModel`.

**Step 4: Run tests to verify they pass**

Run: `cargo test --test copy_integration`

Expected: PASS

**Step 5: Commit**

```bash
git add src/copy.rs tests/copy_integration.rs
git commit -m "feat(progress): replace post-run report"
```

### Task 6: Full Verification

**Files:**
- Modify only if needed: `README.md`

**Step 1: Run repo verification**

Run:
- `cargo fmt --check`
- `cargo clippy --all-targets --all-features -- -D warnings`
- `cargo test`

Expected: PASS

**Step 2: Update docs if examples are now stale**

Make the minimal faithful doc change only if required.

**Step 3: Commit**

```bash
git add README.md src/plan.rs src/lib.rs src/progress_model.rs src/progress_format.rs src/copy.rs tests/plan_layout.rs tests/public_api.rs tests/progress_model.rs tests/progress_format.rs tests/copy_integration.rs
git commit -m "feat(progress): implement canonical 80-col ui"
```
