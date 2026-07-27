# Deterministic quickstart

After installing Medusa, run one command:

```console
medusa quickstart
```

The command performs platform and containment preflight checks, verifies Git and Node.js sidecar prerequisites, detects an authenticated direct provider or a local/custom route, validates the required tool-calling capability, creates a harmless temporary Git repository when `--repo` is not supplied, executes one bounded repository-local proof task, verifies the exact result, and prints a single success or failure report.

Provider and gateway credentials are read only from the process environment. The quickstart flow never writes API keys or gateway credentials to disk.

Supported credential routes are detected from `ANTHROPIC_API_KEY`, `OPENAI_API_KEY`, `MINIMAX_API_KEY`, `MEDUSA_API_KEY`, and `MEDUSA_BASE_URL`. Advanced gateways remain available through `MEDUSA_BASE_URL`, but the quickstart recommends a detected direct provider first.

Use machine-readable output in CI:

```console
medusa quickstart --json
```

Use an existing harmless repository:

```console
medusa quickstart --repo ./sample
```

By default, an automatically-created sample repository is removed after verification. Add `--keep-sample` to retain it for inspection. Every failed check includes the unmet prerequisite and the exact next action before the process exits unsuccessfully.
