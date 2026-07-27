# Product acceptance

The repository has two explicit fail-closed acceptance modes.

## Fast pull-request smoke mode

Run the deterministic smoke contract from the repository root:

```bash
cargo product-acceptance --mode smoke
```

Smoke mode validates the acceptance entrypoint, scenario selection, evidence format, production orchestration, the shipped headless CLI, and the critical platform boundary. On Linux, that boundary uses the real Bubblewrap backend and fails when Bubblewrap is missing or cannot launch.

Ordinary pull requests run smoke mode on Linux. The workflow promotes the run to the complete matrix when acceptance-critical paths change.

## Full product evidence

Run the complete platform suite with:

```bash
cargo product-acceptance --mode full
```

`full` remains the default for local compatibility. It preserves the complete Linux, macOS, and Windows guarantees and can be selected manually with the Product Acceptance workflow dispatch.

Use `--output PATH` with either mode to choose the evidence directory:

```bash
cargo product-acceptance --mode full --output product-acceptance-artifacts
```

## Build reuse

Each runner first executes one Cargo `test --no-run` prebuild for the unique packages selected by the mode. Every scenario then uses the same `CARGO_TARGET_DIR`; runtime logs and temporary evidence remain scenario-specific. Windows therefore reuses compiled artifacts instead of recompiling every scenario solely to avoid executable locks.

The command exits non-zero when the prebuild or any scenario fails. Filtered scenarios also require their test name to appear in captured output, preventing a zero-match filter from being reported as success.

## Evidence

The compact artifact contains:

- `summary.json`, a versioned machine-readable result;
- `build.log`, containing prebuild stdout and stderr;
- one stdout/stderr log per scenario.

`summary.json` records the mode, platform, commit, build duration, combined scenario duration, total job duration, per-scenario duration, and whether shared build reuse was enabled. Cargo target directories are intentionally not uploaded.

## Path-aware full matrix

The full Linux, macOS, and Windows matrix runs automatically when changes affect the acceptance workflow, release gates, Cargo configuration, the acceptance runner, containment, agent tools, durable execution, recovery, rollback, hardening, or this guarantee document. Unrelated pull requests run only the Linux smoke contract. Manual workflow dispatch can run either mode.

## Guarantee ownership

| Guarantee | Authoritative workflow |
| --- | --- |
| Product orchestration, shipped CLI entrypoint, platform containment, interruption/resume, checkpoint restore, repository rollback, escalation, recovery, and byte-exact upgrade/rollback evidence | Product Acceptance |
| Workspace compilation, formatting, linting, and ordinary unit/integration coverage | CI |
| Coverage threshold, adversarial regression set, fuzz/chaos smoke, package smoke, security tools, documentation/schema checks, and live coding release readiness | Release Gates |
| Daemon-specific restart and recovery behavior outside the product-level durable execution scenarios | Daemon |

Release Gates may exercise a narrow test again only when it is part of a distinct release-readiness contract; such duplication must be named in that workflow rather than silently treated as independent product evidence. Product Acceptance is the owner of the cross-platform evidence bundle.

## Platform behavior

Linux CI installs Bubblewrap before running the suite. macOS and Windows run the product contract against their platform implementations. A missing or unsupported containment backend fails closed; neither mode substitutes mocks, weaker backends, or silent skips.

## Review responsibility

The primary implementation or release agent is responsible for reviewing all scenario logs and integrating only validated results. Delegated investigation is not acceptance evidence until the primary agent checks and incorporates it.
