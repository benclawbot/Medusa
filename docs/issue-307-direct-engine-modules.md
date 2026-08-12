# Issue 307 direct engine modules

> Historical record — retained as implementation evidence; it is not current setup or status guidance. Start at [the documentation index](README.md).

This migration replaces `medusa-agent`'s build-time source rewriting with normal Rust source tracked in the repository.

The migration preserves the exact generated engine implementation first, then validates it through formatting, Clippy, dependency policy, security audit, full workspace tests, documentation, and daemon/TUI integration on all configured platforms.

The one-time migration workflows removed themselves after materializing `src/engine.rs`, restoring `autonomous_execution.rs` as a standalone module, and applying canonical Rust 1.88 formatting. `build.rs` is deleted, and production source files remain below the repository's 1,000-line ceiling.

The pull request must use a regular merge commit because the migration and validation fixes are intentionally separate commits.
