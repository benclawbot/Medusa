# Production Security Hardening

Release gates enforce Rust formatting and linting, workspace tests, documentation, dependency policy, vulnerability audit, property-based fuzz smoke, chaos recovery, browser evidence, and package installation smoke.

The hardening layer rejects archive traversal, symlinked state trees, unsafe downgrades, invalid migration transitions, duplicate archive entries, malformed metric names, and credential persistence in operational events.

MCP and skills remain pinned and checksummed. Shell hard-deny rules, environment clearing, repository-relative paths, atomic writes, rollback bundles, and frozen evaluation baselines remain in force.

No CI gate should convert a missing live credential into a passing MiniMax canary. Release managers must run the documented live canary before publishing a signed tag.
