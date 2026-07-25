# Issue 307 direct engine modules

This migration replaces `medusa-agent`'s build-time source rewriting with normal Rust source tracked in the repository.

The migration preserves the exact generated engine implementation first, then validates it through formatting, Clippy, dependency policy, security audit, full workspace tests, documentation, and daemon/TUI integration on all configured platforms.

The pull request must use a regular merge commit because the migration and any validation fixes are intentionally separate commits.
