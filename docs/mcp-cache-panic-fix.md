# MCP cache serialization hardening

> Historical record — retained as implementation evidence; it is not current setup or status guidance. Start at [the documentation index](README.md).

PR #230 removes panic-prone JSON serialization from MCP server configuration and tool-schema fingerprinting.

Both fingerprint paths now handle serialization failure deterministically instead of calling `expect()`. This allows the production panic audit to validate the MCP cache crate and preserves stable SHA-256 fingerprint generation for serializable inputs.
