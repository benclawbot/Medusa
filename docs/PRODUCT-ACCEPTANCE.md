# Product acceptance

Run the authoritative cross-platform product acceptance suite from the repository root:

```bash
cargo product-acceptance
```

Use `--output PATH` to choose the evidence directory:

```bash
cargo product-acceptance --output product-acceptance-artifacts
```

The command exits non-zero when any scenario fails and writes:

- `summary.json`, a versioned machine-readable result;
- one log per scenario containing captured stdout and stderr.

## Product guarantees

The suite validates the production runtime and shipped headless CLI path, platform process containment, durable interruption and resume, checkpoint restoration, repository rollback, bounded escalation, corrupted-state recovery, byte-exact upgrade rollback, and—on Linux—the production Bubblewrap filesystem and network boundary.

Filtered scenarios require their test name to appear in captured output. This prevents an accidentally unmatched Cargo test filter from being reported as success.

## Platform behavior

Linux CI installs Bubblewrap before running the suite. macOS and Windows run the same product contract against their platform containment implementations. A missing or unsupported containment backend must fail closed; the suite does not substitute a weaker execution path.

## Evidence ownership

The primary implementation or release agent is responsible for reviewing all scenario logs and integrating only validated results. Delegated subagents may investigate individual failures, but their output is not acceptance evidence until the primary agent checks and incorporates it.
