---
id: pat-igcu
status: closed
deps: []
links: []
created: 2026-04-09T08:47:12Z
type: bug
priority: 1
assignee: Francis Vidal
tags: [planning, collision]
---
# Replace collision auto-disambiguation with content-aware dedupe


## Notes

**2026-04-09T08:53:02Z**

Replaced template-rewriting collision handling with content-aware dedupe: identical colliding files now collapse to one planned copy; distinct content still returns PlanError::Collision. Verified with cargo fmt --check, cargo clippy --all-targets --all-features -- -D warnings, and cargo test.
