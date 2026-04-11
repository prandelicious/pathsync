---
id: pat-y2yf
status: closed
deps: [pat-p94m, pat-hj5u, pat-mkh5]
links: []
created: 2026-04-11T01:35:04Z
type: task
priority: 3
assignee: Francis Vidal
parent: pat-bmu9
tags: [docs, multi-target, subagent]
---
# Document multi-target job configuration and behavior

Implement Task 7 from docs/plans/2026-04-09-multi-target-job-implementation.md. Update repository docs and examples to describe `target` compatibility, preferred `targets = [..]` syntax, planning fan-out, validation rules, and partial failure semantics.

Suggested subagent: documentation-engineer
Suggested skills: writing-clearly-and-concisely
Primary files: README.md, examples/config.toml, docs/plans/2026-04-09-dual-target-copy-design.md

Delegation notes: stay faithful to implemented behavior; do not invent semantics that the code does not support.

## Acceptance Criteria

- README explains `target` vs `targets` and the validation rules.
- Example config includes a concrete multi-target job.
- Docs state that planning creates one transfer per target root.
- Docs capture the decided partial-failure semantics.
- Edited prose is concrete and consistent with current code paths.


## Notes

**2026-04-11T01:51:58Z**

Updated README, examples/config.toml, and design notes to document target/targets behavior, planning fan-out, and multi-target partial-failure semantics.
