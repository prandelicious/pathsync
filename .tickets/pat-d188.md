---
id: pat-d188
status: closed
deps: [pat-1enr]
links: []
created: 2026-04-11T01:35:04Z
type: task
priority: 2
assignee: Francis Vidal
parent: pat-bmu9
tags: [api, multi-target, rust, subagent]
---
# Update public API for normalized job targets

Implement Task 2 from docs/plans/2026-04-09-multi-target-job-implementation.md. Update public structs and API-facing tests to replace singular target access with normalized `targets`, while preserving backward-compatible single-target behavior.

Suggested subagent: rust-engineer
Suggested skills: test-driven-development, rust-best-practices
Primary files: tests/public_api.rs, src/config.rs, src/lib.rs, src/plan.rs

Delegation notes: scope is API shape and compilation fallout only; do not redesign runtime semantics here.

## Acceptance Criteria

- Public API tests assert `ResolvedJob.targets` access.
- Single-target configs still work through compatibility resolution.
- Multi-target configs preserve both targets in API-visible results.
- Plan building assertions in public API tests reflect one destination per target.
- `cargo test --test public_api` passes.


## Notes

**2026-04-11T01:43:27Z**

Updated public API tests and config literals for normalized ResolvedJob.targets; verified with cargo test --test public_api (7 passed).
