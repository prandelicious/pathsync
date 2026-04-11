---
id: pat-hj5u
status: closed
deps: [pat-vzta]
links: []
created: 2026-04-11T01:35:04Z
type: feature
priority: 1
assignee: Francis Vidal
parent: pat-bmu9
tags: [runtime, copy, multi-target, subagent]
---
# Allow partial multi-target copy failures without aborting the run

Implement Task 5 from docs/plans/2026-04-09-multi-target-job-implementation.md. Preserve best-effort copy execution for multi-target jobs and treat target-local runtime failures as nonfatal to the overall completed run, while keeping planning/config/internal failures fatal.

Suggested subagent: rust-engineer
Suggested skills: systematic-debugging, test-driven-development, rust-best-practices
Primary files: src/copy.rs, tests/copy_integration.rs

Delegation notes: isolate runtime success/error semantics; do not swallow systemic failures or planning errors.

## Acceptance Criteria

- A successful target still receives copied files when another target copy fails.
- Target-local failures are recorded with destination paths.
- The run completes with errors instead of returning fatal run failure for partial target failures.
- Internal/systemic failures remain fatal.
- `cargo test --test copy_integration` passes.


## Notes

**2026-04-11T01:51:58Z**

Multi-target runs now return success for local-only target-specific failures while preserving fatal behavior for single-target and systemic failures. Verified with cargo test --test copy_integration (including multi_target_local_failure_completes_with_errors_but_returns_success).
