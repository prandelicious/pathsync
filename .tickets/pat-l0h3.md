---
id: pat-l0h3
status: closed
deps: []
links: []
created: 2026-03-30T04:55:57Z
type: task
priority: 1
assignee: Francis Vidal
---
# Tighten canonical progress UI to exact original mockup


## Notes

**2026-03-30T05:05:59Z**

Tightened canonical 80-column TTY renderer to the approved mockup by splitting live/post-run summary rows, deriving adaptive display phase from active worker buckets, and switching worker detail columns to match the live mockup. Verified with cargo test --lib, cargo test --test progress_model --test progress_format, cargo clippy --all-targets --all-features -- -D warnings, and a pseudo-TTY run via script against /tmp/pathsync-demo/config.toml.
