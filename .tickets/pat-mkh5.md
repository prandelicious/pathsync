---
id: pat-mkh5
status: closed
deps: [pat-hj5u]
links: []
created: 2026-04-11T01:35:04Z
type: feature
priority: 2
assignee: Francis Vidal
parent: pat-bmu9
tags: [report, ui, multi-target, subagent]
---
# Show target-specific failures in the post-copy report

Implement Task 6 from docs/plans/2026-04-09-multi-target-job-implementation.md. Update post-run model/formatting so completed-with-errors runs still render cleanly and identify which target destination failed.

Suggested subagent: cli-developer
Suggested skills: test-driven-development, writing-clearly-and-concisely
Primary files: src/progress_model.rs, src/progress_format.rs, src/copy.rs, tests/progress_model.rs, tests/progress_format.rs

Delegation notes: preserve the canonical layout as much as possible; prefer better content over broad UI redesign.

## Acceptance Criteria

- Post-run output renders for successful runs that still contain target-local failures.
- Error rows include enough destination detail to identify the failing target.
- Status wording reflects completed-with-errors semantics.
- `cargo test --test progress_model --test progress_format` passes.


## Notes

**2026-04-11T01:51:58Z**

Post-run error rows now retain destination-path detail and formatter uses middle truncation to preserve target context plus final error text. Verified with cargo test --test progress_model --test progress_format and cargo test --test copy_integration.
