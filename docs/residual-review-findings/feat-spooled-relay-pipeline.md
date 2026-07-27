# Known Residuals — feat/spooled-relay-pipeline

Recorded at HEAD `ea82c4e` after a Tier 2 `ce-code-review` pass (10 reviewer personas, run id `20260727-150916-c2b01d1d`) and an independent validator wave for every P0/P1 finding. The user reviewed the residual set and chose **Accept and proceed** — these are shipped knowingly, not silently.

Five actionable findings survived the fix pass (six others were fixed and merged: the `reserve()`/`register()` deadlock-race, lockfile-claim atomicity, cross-job name-prefix collision, `spool` module visibility, `LaneReleaseGuard` scope narrowing, and a stale doc comment). None of the five below are data-loss or silent-success bugs — in every case the run still fails safely and reports correctly; the gaps are narrower and mostly design-judgment calls.

## P1 — `reserve()` has no overall deadline; a permanently hung (non-crashing) target can stall all staging forever

**File:** `src/spool.rs` (the `reserve` function's blocking loop)
**Reviewers:** reliability, adversarial (independently converged; validator-confirmed real)

`reserve()` blocks as long as `live_entry_count > 0` (plus, since the fix, `outstanding_claims > 0`), on the theory that a live entry could eventually be evicted. Nothing verifies actual progress toward that eviction. If a target lane's copy or verify worker hangs on a stuck I/O call (dead network mount, unplugged drive that blocks rather than errors) instead of panicking or erroring, that entry stays "live" indefinitely and `reserve()` waits forever for every other target too — no I/O operation anywhere in the lane copy/verify path has its own timeout.

**Why deferred:** fixing this requires a product decision this session doesn't have standing to make alone — an overall deadline on `reserve()` (and what happens when it fires: fail the whole run? just that one entry?), or per-operation I/O timeouts on the lane copy/verify path, or a liveness/heartbeat mechanism. No existing requirement in the plan specifies desired behavior for "target I/O hangs without erroring."

## P1 — `is_pid_alive` always returns `true` on non-unix, permanently blocking future staged runs after any crash on Windows

**File:** `src/spool.rs`, `is_pid_alive`
**Reviewer:** reliability (validator-confirmed real; validator also noted Windows isn't currently a claimed pathsync target per README/Cargo.toml)

Every lockfile looks "live" on non-unix, so orphan cleanup never triggers there — a crashed staged run permanently blocks all future staged runs of that job on Windows until a human manually deletes the stale spool directory. This directly contradicts the feature's own R8 requirement ("orphaned spool directories from crashed runs are cleaned up on the next run"), but only on a platform the project doesn't currently claim to support.

**Why deferred:** a real fix needs either a new cross-platform dependency (e.g. `windows-sys` `OpenProcess`/`GetExitCodeProcess`) — a scope expansion — or a documented decision to leave Windows staged-mode crash recovery manual. Both are product/scope calls, not mechanical fixes.

## P2 — `~640`-line staged orchestration block in `copy.rs` wasn't extracted into its own module

**File:** `src/copy.rs` (`StagingTask` → `run_copy_staged`, roughly lines 489–1132)
**Reviewer:** maintainability (confidence 100)

This PR established a one-module-per-pipeline-stage pattern (`spool.rs`, `stage.rs`, `lanes.rs`) but left the orchestration/scheduling layer inline in the already-large `copy.rs`.

**Why deferred:** a real structural refactor this late carries regression risk disproportionate to the benefit (it's a code-quality nicety, not a bug) — better done deliberately, with its own review pass, than folded into an already-large fix batch.

## P2 — New additive struct fields break external library consumers' struct-literal construction; no version bump

**Files:** `src/config.rs` (`ResolvedJob.staging`), `src/progress_model.rs` (`LiveScreenModel`/`PostRunScreenModel` staging fields)
**Reviewer:** api-contract (confidence 75)

The new `staging: Option<...>` fields are additive at the type level but source-breaking for any external consumer constructing these structs via literal (proven by the fact `tests/public_api.rs`, the crate's own contract suite, needed `staging: None` added at every existing literal site). `Cargo.toml` stays at `0.1.0`.

**Why deferred:** whether/when to bump the version is a release-policy decision, not a code fix — pathsync is pre-1.0, where looser semver expectations are normal, but the call belongs to whoever owns the release cadence.

## P3 — Source-released milestone can silently never fire if a staging worker panics

**File:** `src/copy.rs` (`StagingReleaseTracker`, `run_staging_standard`/`run_staging_adaptive`)
**Reviewer:** adversarial (downgraded from an initially-reported P0 after independent validation — see below)

Neither staging scheduler calls `tracker.note_terminal()` on a worker panic, so `WorkerEvent::SourceReleased` may never fire for the rest of a run if a staging task's worker panics. Validator-confirmed the impact is low: `SourceReleased` is a UI/report banner, not a completion gate — the render loop still exits correctly via channel disconnect, and the run still fails with the correct exit code via the existing `handle.join().is_err()` → systemic-failure path regardless of whether the banner ever printed.

**Why deferred:** low-priority UX gap, not a correctness bug; the proper fix is symmetric across both schedulers and better bundled with any future panic-handling pass on the staging layer rather than rushed now.

---

## Also noted but never actionable (advisory-only, no fix owed)

- `StageReleaseGuard`'s disarm point (`src/stage.rs`) is disarmed slightly before `stage_file`'s own explicit `register`/`mark_terminal` cleanup runs, leaving that narrow window technically unprotected against a panic — but the unprotected code is plain mutex/HashMap operations with no `unwrap`/indexing, so the practical risk is very low (adversarial, confidence 50).
- `group_failures()` in `src/copy.rs` is an O(n²) linear scan over `report.failures` with no size cap — real shape, but impact is unproven at realistic failure-list sizes (performance, confidence 50).
- `StagingReleaseTracker`'s exactly-once guarantee is only tested with two sequential calls on one thread, not under genuine multi-thread contention (testing, confidence 50).
- Assorted testing gaps: no test for the cross-job prefix-collision fix under real filesystem timing beyond the deterministic unit test already added; no `bench-copy`-measured throughput cost of the source→spool inline-hashing path vs. the native fast path; no behavioral (non-compile-only) test of `ResolvedJob.staging` through `tests/public_api.rs`.

## One P0 that was reported and rejected by independent validation

A reviewer initially flagged `run_staging_standard`'s lack of per-task `catch_unwind` (unlike `run_staging_adaptive`, which has it) as a P0 — "silently strands files with no failure reported." An independent validator found this overstated: the shared work queue means surviving workers keep draining it after one dies (full stranding only happens at `worker_count == 1`), the pattern faithfully mirrors the pre-existing `execute_phase`/`worker_loop` scheduler that predates this feature, and the outer `handle.join().is_err()` check still correctly reports a systemic failure and non-zero exit either way. Downgraded to the P3 above (the `SourceReleased`-banner gap is the only real residual consequence). Not re-listed as a separate item.
