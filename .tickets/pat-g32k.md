---
id: pat-g32k
status: closed
deps: []
links: []
created: 2026-03-30T02:18:38Z
type: epic
priority: 1
assignee: Francis Vidal
tags: [ui, progress, renderer]
---
# Design canonical 80-col progress UI renderer


## Notes

**2026-03-30T02:19:54Z**

Design doc: docs/plans/2026-03-30-canonical-progress-ui-design.md. Implementation plan: docs/plans/2026-03-30-canonical-progress-ui-implementation.md. Canonical contract is exact 80-column parity for live and post-run screens.

**2026-03-30T04:49:24Z**

Implementation is in /Users/francis/Developer/projects/pathsync/.worktrees/canonical-progress-ui on branch codex/canonical-progress-ui. Verified with cargo test and cargo clippy --all-targets --all-features -- -D warnings. Live TTY now redraws canonical 80-column frames using planning stats and screen models.
