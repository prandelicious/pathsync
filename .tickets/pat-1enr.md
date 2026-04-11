---
id: pat-1enr
status: closed
deps: []
links: []
created: 2026-04-11T01:35:04Z
type: feature
priority: 1
assignee: Francis Vidal
parent: pat-bmu9
tags: [config, multi-target, rust, subagent]
---
# Add multi-target config resolution and validation

Implement Task 1 from docs/plans/2026-04-09-multi-target-job-implementation.md. Extend config parsing to support backward-compatible `target` and new `targets`, normalize into `ResolvedJob.targets: Vec<PathBuf>`, and add typed validation errors.

Suggested subagent: rust-engineer
Suggested skills: test-driven-development, rust-best-practices
Primary files: src/config.rs, src/error.rs, tests/config_date.rs

Delegation notes: keep the change narrowly focused on config and resolution; do not fan out planning or runtime behavior in this ticket.

## Acceptance Criteria

- `target` resolves to a one-element `ResolvedJob.targets`.
- `targets` resolves in-order to multiple targets.
- Both set, neither set, and empty `targets` produce typed config errors.
- Missing target directories still surface a path-specific config error.
- `cargo test --test config_date` passes.


## Notes

**2026-04-11T01:40:20Z**

Implemented target/targets normalization into ResolvedJob.targets with typed config validation errors and preserved TargetFolderNotFound for missing directories. Verified with cargo test --test config_date (20 passed).
