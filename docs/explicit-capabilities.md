# Explicit production capabilities

`medusa-capabilities` is the shared authority for capability discovery, permission grants, explicit approvals, diagnostics, and audit events used by the CLI, TUI, desktop runtime, and agent prompt context.

## GitHub operations

GitHub reads and writes are routed through `medusa-github`. Read permission is required before authentication or repository queries execute. Writes require an explicit approval before any `gh` or `git` command is invoked.

Each authorization decision records the capability, requested permission, approval state, outcome, and reason. Denied permissions fail before an external command or other side effect.

### Create a repository

`medusa-capabilities create-repository` is the supported non-interactive repository-creation entrypoint. It prints the complete typed request before execution and requires `--approve` because the operation creates an external persistent resource.

```bash
medusa-capabilities create-repository \
  --owner acme \
  --name example \
  --visibility private \
  --description "Example service" \
  --default-branch main \
  --add-readme \
  --disable-wiki \
  --approve
```

Create a repository from an existing or new local project:

```bash
medusa-capabilities create-repository \
  --owner acme \
  --name example \
  --source ./example \
  --initialize-git \
  --initial-commit-message "Initial commit" \
  --approve
```

The creation request supports public, private, and internal visibility; descriptions and homepages; default branches; README, `.gitignore`, and license initialization; template repositories; issue/wiki settings; GitHub Enterprise hostnames; and explicit idempotent reuse.

Local bootstrap validates the destination before creating the remote, refuses an unrelated `origin`, initializes Git only when requested, creates an initial commit only when requested or when Medusa must materialize an otherwise empty remote, sets the requested branch, and pushes with upstream tracking. A partially completed operation reports the created repository URL and instructs the caller to retry with `--reuse-existing` after correcting the local failure.

Successful execution emits a JSON receipt containing the canonical repository identity, web and clone URLs, visibility, actual default branch, whether the repository was newly created, the local path, and the initial commit when applicable. The receipt and authorization events are appended to `.medusa/audit/github-repository-creation.jsonl` (or the configured `MEDUSA_HOME`). Command arguments are passed directly without a shell, credentials remain owned by the GitHub CLI credential store, and credential-like stderr is redacted.

The following combinations fail before external mutation:

- invalid owner, repository, branch, URL, or template identifiers;
- template creation mixed with README, license, `.gitignore`, or local-source creation;
- local-source creation mixed with remote initialization files;
- a missing/non-Git source when `--initialize-git` was not selected;
- an existing repository without `--reuse-existing`;
- an unrelated existing `origin` remote;
- repository mutation without `--approve`.

See [GitHub repository creation](GITHUB-REPOSITORY-CREATION.md) for modes, recovery, and examples.

## Self-improvement

`medusa-improvement` remains proposal and evaluation logic; it does not receive a direct repository mutation path. A proposed improvement includes:

- evidence-backed rationale;
- a reviewable unified diff;
- verification commands;
- rollback metadata;
- touched paths and safety analysis.

The runtime derives changed paths from the submitted unified diff and requires them to exactly match the declared touched paths. Duplicate pending proposal IDs are rejected so reviewed work cannot be replaced under an existing transaction identifier.

Approved improvements pass through `medusa-transaction-coordinator`. A successful verification callback must provide evidence for every declared verification command before the prepared vote is cast. Repository mutation is supplied only as the final approved transaction callback, and a commit failure invokes an executable rollback callback before the transaction is marked failed.

Changes to policy, sandbox, approval, credential, capability, hardening, workflow, or update-trust paths require separate sensitive-change approval before a transaction can be staged. This includes the concrete approval, policy, Windows sandbox, and desktop credential implementations.

## Diagnostics

The shared capability matrix exposes both `GitHub` and `Self-improvement`, including availability details. Runtime diagnostics also expose capability descriptors and the ordered authorization audit trail. `medusa-capabilities [repository]` remains backward-compatible and emits these diagnostics as JSON; `MEDUSA_GITHUB_REPOSITORY` selects the GitHub repository identity used by diagnostics.

## Code intelligence

The registry exposes a dedicated `CodeIntelligence` capability for the production `semantic_capabilities`, `code_index`, and guarded `symbol_rename` tools. The capability report distinguishes production, partial, and unavailable levels independently for text search, parsed symbols, definitions, references, diagnostics, workspace symbols, and guarded refactoring.

TypeScript/JavaScript LSP normalization remains below the production boundary until a server lifecycle, dependency probe, dispatcher, freshness contract, and v2 mutation transaction are certified. It is therefore reported as text-only rather than as full indexing. See [Code-intelligence capability levels](CODE-INTELLIGENCE-CAPABILITIES.md).
