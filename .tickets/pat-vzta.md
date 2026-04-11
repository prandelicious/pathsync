---
id: pat-vzta
status: closed
deps: [pat-1enr]
links: []
created: 2026-04-11T01:35:04Z
type: feature
priority: 1
assignee: Francis Vidal
parent: pat-bmu9
tags: [planning, multi-target, rust, subagent]
---
# Expand planning across target roots

Implement Task 3 from docs/plans/2026-04-09-multi-target-job-implementation.md. Change planning so one scanned source file expands to one `TransferPlan` per target root, while preserving a single source scan, deterministic ordering, and collision detection keyed by final destination path.

Suggested subagent: rust-engineer
Suggested skills: test-driven-development, rust-best-practices
Primary files: src/plan.rs, src/lib.rs, tests/plan_layout.rs

Delegation notes: keep skip logic and planning stats target-aware; do not change runtime failure semantics in this ticket.

## Acceptance Criteria

- One source file under a multi-target job yields one planned transfer per target.
- Templated layouts render once and fan out to every target root.
- Planning stats count expanded transfers and bytes correctly.
- Skip/collision handling is evaluated per final destination path.
- `cargo test --test plan_layout` passes.


## Notes

**2026-04-11T01:43:27Z**

Expanded planning across target roots with per-destination skip accounting and deterministic fan-out. Verified with cargo test --test plan_layout (10 passed).
