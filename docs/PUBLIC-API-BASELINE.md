# Public API Baseline

This file defines the compatibility surface that the modularization refactor must preserve.

## Rust crates

The following workspace crates are treated as externally observable boundaries even when they are currently consumed only inside the workspace:

- `medusa-core`
- `medusa-provider`
- `medusa-memory`
- `medusa-intelligence`
- `medusa-agent`
- `medusa-workers`
- `medusa-extensions`
- `medusa-hardening`
- `medusa-daemon`
- `medusa-cli`
- `medusa-tui`

Moving an existing public item between internal modules is allowed only when its original public path remains available through a narrow re-export.

## Stable behavioral surfaces

The refactor must preserve:

- CLI command names, flags, exit behavior, and help-visible descriptions;
- daemon request and response schemas;
- durable session, job, memory, migration, receipt, and evidence formats;
- tool names, JSON input schemas, and output/error categories;
- public error codes and policy-denial semantics;
- provider configuration and MiniMax request behavior;
- browser, hook, skill, and MCP evidence contracts;
- package names and the `medusa` binary name.

## Change protocol

An intentional compatibility change requires all of the following in the same pull request:

1. a migration note under `docs/`;
2. compatibility or migration tests covering old and new forms;
3. an update to this baseline;
4. a pull-request section titled `Public API change`;
5. passing release gates.

Absent that protocol, API drift is a regression.

## Verification

Each extraction pull request must run:

```bash
cargo test --workspace --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps
```

The API baseline will later be upgraded to a generated `cargo public-api` or semver-check snapshot once the crate publishing surface is finalized. Until then, compatibility is enforced by existing integration, serialization, migration, CLI, package, and live-provider tests plus review of public re-exports.
