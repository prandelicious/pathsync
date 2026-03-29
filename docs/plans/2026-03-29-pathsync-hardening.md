# `pathsync` Hardening Execution Plan

## Summary
- Save this plan as `docs/plans/2026-03-29-pathsync-hardening.md`.
- Execution model: one coordinator agent plus parallel worker sub-agents.
- All worker sub-agents use `agent_type=worker`, `model=gpt-5.4-mini`, `reasoning_effort=medium`.
- Goal: harden correctness first, then integrate docs and quality gates without overlapping write sets.

## Public Interface Changes
- `compare.mode` gains `size_mtime`.
- Default compare behavior changes from `path_size` to `size_mtime`.
- Planning becomes strict on destination collisions: any two sources resolving to the same target path is a hard error.
- Rendered layout output must stay within the target root; templates producing absolute or escaping paths are rejected.
- `cargo run -- --help` works via `default-run = "pathsync"`.

## Execution Order
1. Coordinator setup:
   - Create an isolated worktree and feature branch.
   - Capture baseline with `cargo test`, `cargo clippy --all-targets --all-features -- -D warnings`, and `cargo run --bin pathsync -- --help`.
   - Create `src/lib.rs` as the integration target and define module boundaries only: `config`, `date`, `plan`, `copy`.
2. Batch 1 in parallel:
   - Worker A owns config/date behavior.
   - Worker B owns layout/planning/collision behavior.
   - Worker C owns package/docs/examples.
3. Coordinator integration:
   - Merge Worker A and B modules into `src/lib.rs`.
   - Thin `src/main.rs` down to CLI parsing and library invocation.
   - Add CLI help text and fix current clippy failures in runtime orchestration.
4. Batch 2 in parallel:
   - Worker D owns integration tests for dry-run, copy, skip, and metadata preservation.
   - Worker E owns follow-up polish on error messages and any missing unit tests discovered during integration.
5. Coordinator verification:
   - Run full checks again.
   - Resolve conflicts or regressions.
   - Request review before merge.

## Parallel Tasks

### Worker A: Config and Date Semantics
- Ownership:
  - `src/config.rs`
  - `src/date.rs`
  - `tests/config_date.rs`
- Deliverables:
  - Move config parsing and compare/transfer resolution out of `src/main.rs`.
  - Add `size_mtime` compare mode and make it the default when omitted.
  - Replace `chrono_like` with `chrono`-backed local-time fallback from file mtime.
  - Preserve filename-first date extraction for `YYYYMMDD_HHMMSS`.
- Test cases:
  - Missing `compare.mode` resolves to `size_mtime`.
  - Explicit `path` and `path_size` still behave unchanged.
  - Filename-derived date wins over mtime fallback.
  - Mtime fallback uses local calendar date.

### Worker B: Planning, Layout Safety, and Collisions
- Ownership:
  - `src/plan.rs`
  - `tests/plan_layout.rs`
- Deliverables:
  - Move plan-building, rendered-path generation, and skip-decision helpers out of `src/main.rs`.
  - Detect all destination collisions during planning and return a structured error listing destination plus all conflicting sources.
  - Reject rendered paths that are absolute, contain unresolved tokens, contain `..`, or normalize outside the target root.
  - Implement `size_mtime` skip comparison using byte length plus whole-second mtime equality.
- Test cases:
  - Two sources targeting one destination fail planning.
  - Valid relative templates pass.
  - Absolute and escaping templates fail.
  - `size_mtime` skips unchanged files and recopies changed ones.

### Worker C: Package, Docs, and Example Config
- Ownership:
  - `Cargo.toml`
  - `README.md`
  - `examples/config.toml`
- Deliverables:
  - Add `default-run = "pathsync"`.
  - Add any new dependency declarations needed by Worker A.
  - Document config keys, new default compare behavior, collision failure behavior, and template safety rules.
  - Update the example config to show `size_mtime` as the recommended compare mode.
- Test cases:
  - `cargo run -- --help` resolves the correct binary.
  - Docs examples match the implemented config names and defaults.

### Worker D: End-to-End Integration Tests
- Ownership:
  - `tests/copy_integration.rs`
- Start condition:
  - Begins only after coordinator has integrated `src/lib.rs` and stabilized callable APIs.
- Deliverables:
  - Add temp-dir integration coverage for dry-run, real copy, rerun skip behavior, collision failure, and metadata preservation.
- Test cases:
  - Dry-run reports planned operations and performs no writes.
  - Real copy preserves contents and mtime.
  - Rerun under `size_mtime` skips unchanged files.
  - Planning failure occurs before copy when collisions exist.

### Worker E: Gap-Fill and Verification Support
- Ownership:
  - Additional tests only, or small isolated helper modules the coordinator assigns after Batch 1.
- Start condition:
  - Spawn only after Batch 1 review identifies a missing edge case.
- Deliverables:
  - Cover any missed path-normalization or error-surface edge cases without editing Worker A/B files unless explicitly reassigned.

## Coordinator Responsibilities
- Keep workers on disjoint write sets; do not let two workers edit the same file.
- Review each worker diff before integration.
- Perform all shared-file edits:
  - `src/lib.rs`
  - `src/main.rs`
- During integration:
  - Re-export only the minimal API needed by CLI and tests.
  - Keep copy execution logic behaviorally unchanged except for clippy cleanup and stronger correctness checks.
  - Ensure dry-run and normal execution surface the same planning errors.
- After each batch:
  - Run verification.
  - Fix integration breakage locally rather than bouncing it back unless a worker’s module is fundamentally wrong.

## Sub-Agent Prompt Template
- Use this shape for every worker:
  - “You are Worker X. You are not alone in the codebase. Other workers may be editing different files; do not revert their work. Own only these files: [...]. Implement only the assigned scope. Run relevant tests for your scope. Return a short summary, verification performed, and the exact files changed.”
- Spawn config:
  - `agent_type=worker`
  - `model=gpt-5.4-mini`
  - `reasoning_effort=medium`

## Verification
- Batch 1:
  - Targeted tests for each worker-owned test file.
- Coordinator integration:
  - `cargo test`
  - `cargo clippy --all-targets --all-features -- -D warnings`
  - `cargo run -- --help`
- Pre-merge review:
  - Re-run full verification after any review fixes.

## Assumptions and Defaults
- Non-UTF-8 filenames remain unsupported.
- Collision handling is strict-fail only in this pass; no auto-rename policy.
- Worker C may add `chrono` to dependencies, but Worker A owns the runtime date behavior.
- If `README.md` does not exist, Worker C creates it; otherwise Worker C updates the existing file.
