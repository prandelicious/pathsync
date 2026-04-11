---
id: pat-p94m
status: closed
deps: [pat-d188, pat-vzta]
links: []
created: 2026-04-11T01:35:04Z
type: task
priority: 2
assignee: Francis Vidal
parent: pat-bmu9
tags: [output, dry-run, multi-target, subagent]
---
# Make job summaries and dry-run output target-aware

Implement Task 4 from docs/plans/2026-04-09-multi-target-job-implementation.md. Update orchestration output so list-jobs and dry-run displays clearly show all target roots and per-target transfer mappings.

Suggested subagent: cli-developer
Suggested skills: test-driven-development, writing-clearly-and-concisely
Primary files: src/lib.rs, src/copy.rs, tests/public_api.rs, tests/copy_integration.rs

Delegation notes: keep formatting changes minimal and unambiguous; do not modify post-run summary semantics beyond what is needed for multi-target visibility.

## Acceptance Criteria

- Job summary output lists all target roots clearly for multi-target jobs.
- Dry-run output prints one source->destination line per target-specific transfer.
- `no new files to copy` only appears when all target-specific transfers are skipped.
- `cargo test --test public_api --test copy_integration` passes.


## Notes

**2026-04-11T01:51:57Z**

Made list-jobs output target-aware for multi-target jobs and added dry-run/list-jobs integration coverage. Verified with cargo test --test copy_integration and cargo test --test public_api --test copy_integration.
