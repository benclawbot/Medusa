# Issue 307 source-splicing removal

This branch removes the first hidden build edge from `medusa-agent`:

- `medusa-multi-agent-scheduler` is now a normal Cargo dependency.
- `medusa-agent/build.rs` no longer reads or embeds the scheduler crate's `src/lib.rs`.
- autonomous execution still compiles through the existing generated engine while the remaining engine transformations are migrated in later changes.

Validation requires formatting, Clippy, full workspace tests, documentation, dependency-policy checks, and daemon/TUI integration on all configured platforms.
