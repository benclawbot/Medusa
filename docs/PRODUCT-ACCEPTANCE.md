# Product acceptance

Medusa uses two explicit product-acceptance modes. Pull requests receive fast deterministic feedback without weakening the authoritative release evidence generated after merge.

## PR smoke acceptance

Every non-draft pull request runs the Linux smoke contract:

```bash
python3 scripts/product-acceptance-smoke.py --output product-acceptance-smoke-artifacts
```

The smoke contract exercises production orchestration, the shipped headless entrypoint, checkpoint restoration, repository rollback, the real Bubblewrap filesystem/network/process boundary, and interruption/resume. All scenarios share one `CARGO_TARGET_DIR`, so the workspace graph is compiled once and reused instead of being rebuilt in isolated per-scenario target directories.

The command exits non-zero when any scenario fails and writes:

- `summary.json`, a versioned machine-readable result with `mode: pr-smoke`;
- one stdout/stderr log per scenario.

Filtered scenarios still require their exact test marker in captured output. A Cargo command that exits successfully after matching zero tests therefore cannot be counted as acceptance evidence.

## Full cross-platform evidence

Run the authoritative cross-platform suite from the repository root:

```bash
cargo product-acceptance
```

Use `--output PATH` to choose the evidence directory:

```bash
cargo product-acceptance --output product-acceptance-artifacts
```

The complete Linux, macOS, and Windows matrix runs for every commit merged to `main` and through manual workflow dispatch. This preserves the full production runtime, platform containment, durable interruption and resume, checkpoint restoration, repository rollback, bounded escalation, corrupted-state recovery, and byte-exact upgrade rollback contract.

The authoritative command exits non-zero when any scenario fails and writes:

- `summary.json`, a versioned machine-readable result;
- one log per scenario containing captured stdout and stderr.

## Evidence model

PR smoke is an early feedback layer, not a substitute for full evidence. A pull request cannot claim cross-platform acceptance from the smoke result. The merged commit is the provenance anchor for the full matrix and its uploaded artifacts.

Linux CI installs Bubblewrap before either mode runs. macOS and Windows execute only in the full evidence matrix against their platform containment implementations. Missing or unsupported containment must fail closed; neither mode substitutes mocks or a weaker execution path.

The primary implementation or release agent is responsible for reviewing all scenario logs and integrating only validated results. Delegated investigations are not acceptance evidence until the primary agent checks and incorporates them.
