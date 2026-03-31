# Console Progress UI Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Replace the current progress output with the approved `indicatif` + `console` live monitor and post-run report while preserving best-effort copy behavior and non-TTY plain-text fallback.

**Architecture:** Keep `src/copy.rs` as the integration/orchestration layer. Push render text and layout helpers into `src/progress_model.rs` and `src/progress_format.rs` so the live TTY layout, plain-text fallback, and post-run summary can evolve independently and be tested directly. Use `indicatif` for progress bars/spinners and `console` for color and width-aware static/report text.

**Tech Stack:** Rust, `indicatif`, `console`, existing integration tests, `cargo test`, `cargo clippy`, `cargo fmt`

---

### Task 1: Add Progress Layout Model Helpers

**Files:**
- Modify: `src/progress_model.rs`
- Test: `tests/progress_model.rs`

**Owner:** Worker A (`rust-engineer`)

**Step 1: Write the failing tests**

Add or extend pure-model tests for:
- live status label returns `LIVE / COPY-LARGE`, `LIVE / COPY-SMALL`, or `LIVE / COPY`
- post-run status label returns `COMPLETE` or `COMPLETE WITH ERRORS`
- overall bar label returns `Total copy progress` while work is active
- post-run bar label returns `Copy completion`
- overall status text stops using generic `copying` and exposes explicit phase-aware wording

**Step 2: Run tests to verify they fail**

Run: `cargo test --test progress_model`
Expected: FAIL on the new layout/status assertions.

**Step 3: Write minimal implementation**

Update `src/progress_model.rs` to add pure helpers for:
- live mode title/status wording
- post-run title/status wording
- explicit bar labels
- phase-aware status text for adaptive and standard transfers

Keep the model focused on text/state decisions, not ANSI styling.

**Step 4: Run tests to verify they pass**

Run: `cargo test --test progress_model`
Expected: PASS

**Step 5: Commit**

```bash
git add src/progress_model.rs tests/progress_model.rs
git commit -m "feat(progress): add console ui status model"
```

### Task 2: Add Console-Oriented Formatting Helpers

**Files:**
- Modify: `src/progress_format.rs`
- Modify: `Cargo.toml`
- Test: `tests/progress_format.rs`

**Owner:** Worker B (`worker` with CLI-formatting scope)

**Step 1: Write the failing tests**

Add or extend pure-format tests for:
- worker prefixes render as bare `W01`, `W02` without brackets
- worker labels still disambiguate duplicate basenames using relative paths
- worker line formatting supports `progress + current item + size + rate + time`
- idle worker line renders a stable `idle` row without bogus numeric fields
- plain-text overall line uses the new `Total copy progress` wording

**Step 2: Run tests to verify they fail**

Run: `cargo test --test progress_format`
Expected: FAIL on the new formatting assertions.

**Step 3: Write minimal implementation**

Update `src/progress_format.rs` and `Cargo.toml` to:
- add `console` dependency
- provide formatting helpers for:
  - worker prefix labels
  - worker message layout
  - plain-text progress line wording
  - post-run section headers/dividers with console-aware styling hooks kept optional at the formatting boundary
- keep Unicode width handling conservative; do not assume perfect alignment for arbitrary filenames

**Step 4: Run tests to verify they pass**

Run: `cargo test --test progress_format`
Expected: PASS

**Step 5: Commit**

```bash
git add Cargo.toml src/progress_format.rs tests/progress_format.rs
git commit -m "feat(progress): add console formatting helpers"
```

### Task 3: Update Integration Coverage for the New UI Contract

**Files:**
- Modify: `tests/copy_integration.rs`

**Owner:** Worker C (`test-automator` equivalent via `worker`)

**Step 1: Write the failing tests**

Extend `tests/copy_integration.rs` to assert:
- non-TTY output uses `Total copy progress` instead of old generic wording
- adaptive runs no longer report `phase    : adaptive` when the live UI/reporting chooses explicit copy phase wording
- live/plain worker rows use `W01`, `W02` style labels instead of bracketed prefixes
- final report uses `COMPLETE` or `COMPLETE WITH ERRORS`
- failure paths still never print `all copies complete`

Keep assertions resilient to progress timing by checking stable substrings rather than full output snapshots.

**Step 2: Run tests to verify they fail**

Run: `cargo test --test copy_integration`
Expected: FAIL on the new UI wording assertions.

**Step 3: Write minimal test-only adjustments**

Update the test fixtures or helpers only if necessary to make the new output assertions deterministic.

**Step 4: Run tests to verify they still fail for missing implementation**

Run: `cargo test --test copy_integration`
Expected: FAIL only on missing production behavior, not on broken test setup.

**Step 5: Commit**

```bash
git add tests/copy_integration.rs
git commit -m "test(progress): define console ui integration contract"
```

### Task 4: Implement Live TTY Monitor in Copy Flow

**Files:**
- Modify: `src/copy.rs`

**Owner:** Coordinator (`rust-engineer` or local)

**Step 1: Run the targeted failing tests**

Run:
- `cargo test --test progress_model`
- `cargo test --test progress_format`
- `cargo test --test copy_integration`

Expected: FAIL on the new UI contract before implementation.

**Step 2: Write minimal implementation**

Update `src/copy.rs` to:
- switch the TTY branch to the approved `indicatif` + `console` layout
- render a live title block with explicit `LIVE / COPY-*` status
- render the overall bar using `Total copy progress`
- render worker rows as `spinner + WNN + bar + message columns`
- remove the old `Impact`/legacy meter language entirely
- keep `MultiProgress` worker counts stable per phase; do not rely on cursor movement when bar counts change

Do not break existing best-effort failure handling or adaptive scheduling behavior.

**Step 3: Run targeted tests to verify they pass**

Run:
- `cargo test --test progress_model`
- `cargo test --test progress_format`
- `cargo test --test copy_integration`

Expected: PASS

**Step 4: Refine post-run report**

Still in `src/copy.rs`, update the final summary/report to:
- use `COMPLETE` / `COMPLETE WITH ERRORS`
- use `Copy completion`
- preserve failure classifications and copied file previews
- keep plain-text output readable without ANSI dependence

**Step 5: Re-run integration tests**

Run: `cargo test --test copy_integration`
Expected: PASS

### Task 5: Full Verification and Cleanup

**Files:**
- Modify only if needed: `README.md`

**Owner:** Coordinator

**Step 1: Run repo verification**

Run:
- `cargo fmt --check`
- `cargo clippy --all-targets --all-features -- -D warnings`
- `cargo test`

Expected: PASS

**Step 2: Update docs if user-facing output examples changed materially**

If README examples or progress wording are now stale, make the minimal faithful update.

**Step 3: Commit**

```bash
git add Cargo.toml README.md src/copy.rs src/progress_model.rs src/progress_format.rs tests/progress_model.rs tests/progress_format.rs tests/copy_integration.rs
git commit -m "feat(progress): implement console progress ui"
```

## Parallel Dispatch Notes

- Batch 1 can run in parallel:
  - Worker A owns `src/progress_model.rs` and `tests/progress_model.rs`
  - Worker B owns `src/progress_format.rs`, `Cargo.toml`, and `tests/progress_format.rs`
  - Worker C owns `tests/copy_integration.rs`
- Batch 2 is coordinator-only:
  - `src/copy.rs` is the integration seam and should stay on the critical path
- Batch 3 is verification and optional docs touch

## Reviewer Checklist

- Live TTY output uses `indicatif` + `console`, not the old generic bar wording
- Worker rows use spinner + `WNN` + bar + structured message text
- Non-TTY output remains readable and automation-friendly
- Adaptive runs report explicit copy phase wording rather than opaque `adaptive`
- Failure paths never print `all copies complete`
- Final report uses the new completion language without losing failure details
