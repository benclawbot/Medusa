# Workspace modes

Medusa operates on a bounded **workspace root**. A workspace may be a Git repository, an ordinary directory, or a Medusa-owned ephemeral directory. Git is therefore a capability of one workspace backend, not a prerequisite for every Medusa session.

## Supported modes

| Mode | Detection / creation | Read-only teammate work | Mutation isolation | Parallel mutating implementers | Durable acceptance unit |
|---|---|---|---|---|---|
| **Git workspace** | Existing Git work tree | Yes | Dedicated Git worktree per implementer | Yes, when the conflict-aware mutation DAG accepts the decomposition | Prepared commit/tree plus typed receipts |
| **Directory workspace** | Existing non-Git directory | Yes | Isolated content-addressed directory snapshot | No; one isolated implementer is used | Content-addressed snapshot/tree plus typed receipts |
| **Ephemeral workspace** | Created explicitly through `Workspace::ephemeral()` | Yes | Same snapshot backend as a directory workspace | No | Content-addressed snapshot/tree plus typed receipts until explicit cleanup |

The shared `RuntimeController`, provider routing, session lifecycle, teammate coordination, review, verification, and recovery authorities are the same across workspace modes. Only the mutation-storage and integration backend changes.

## User surfaces

Workspace semantics are shared across every production frontend rather than implemented separately by each UI.

| Surface | Workspace entry | Non-Git behavior |
|---|---|---|
| **Headless CLI** | `medusa --repo /path/to/workspace run ...` | The compatibility-named `--repo` argument is resolved as a filesystem root and passed directly to `RuntimeController`; `.git` is not required. |
| **TUI** | `medusa --repo /path/to/workspace` or launch from the current directory | `TuiOptions` scopes daemon/session state under `<workspace>/.medusa` and uses the same runtime backend. |
| **Desktop** | Selected directory, or no directory for General Chat | `runtime_start` canonicalizes any directory. General Chat creates a Medusa-owned non-Git application-data workspace. |
| **Daemon** | Workspace root supplied by CLI, TUI, Desktop, or Telegram | `DaemonPaths` scopes IPC and durable state under `<workspace>/.medusa/daemon`; startup does not inspect Git metadata. |
| **Telegram** | The workspace root used to start `medusa telegram` | Telegram creates transport state under `<workspace>/.medusa/telegram` and attaches to the same workspace-scoped daemon. |

Git-specific actions remain capability-specific. For example, creating branches, commits, pull requests, Git diffs, or GitHub repository operations require a Git workspace even though the conversation, research, documentation, file, artifact, and directory-mutation paths do not.

The repository contains an executable `scripts/check-workspace-surfaces.py` conformance gate. It verifies that all five entrypoints continue handing a filesystem root to the shared runtime/daemon without introducing a Git-only startup requirement. The Workspace Backend Certification runs this gate together with backend tests.

## Git workspace mutation

Git-backed mutation keeps Medusa's strongest coding workflow. Low- or medium-risk implementation scope may be decomposed into a conflict-aware DAG when all of the following are true:

- the plan has at least two exact file ownership scopes;
- no task claims the whole repository or a directory-level ambiguous write scope;
- the decomposition is above the production confidence threshold;
- the task count fits the bounded mutator budget (currently at most three);
- resource conflicts such as manifests, lockfiles, migrations, snapshots, and generated outputs do not make a concurrent wave unsafe.

Each child implementer receives its own Git worktree and exact write contract. Child results are scope-checked and verified independently. The runtime establishes a deterministic integration barrier, composes accepted children in dependency order into an aggregate worktree, verifies that aggregate, and sends the immutable aggregate through the dedicated parent-review, independent-verification, authorization, integration, and reconciliation lifecycle. Workers never integrate their own changes.

If the decomposition is unsafe or ambiguous, Medusa automatically falls back to one isolated implementer. High-risk mutation stays single-implementer.

## Directory workspace mutation

An ordinary directory does not need `.git` and does not require Git to be installed for runtime work. The directory backend:

1. fingerprints the bounded workspace into a deterministic content revision;
2. copies that revision into an isolated worker directory under Medusa execution state;
3. confines the implementer to its task contract and validates the exact changed components;
4. removes Medusa/runtime residue before accepting the candidate;
5. stores the accepted candidate as an immutable content-addressed snapshot;
6. presents a bounded text-or-digest patch to the same zero-tool parent reviewer;
7. materializes the immutable snapshot separately for independent verification;
8. rejects integration if the primary directory changed after planning;
9. applies only the authorized changed paths and restores originals if integration fails;
10. verifies that the resulting directory tree exactly matches the authorized snapshot.

Directory mutation fails closed on symbolic links because copying a symlink-bearing tree without Git's exact object semantics can create path-confusion or escape risks. Use a Git workspace for symlink-bearing mutation.

Parallel **mutating** implementers are intentionally Git-only in this release because the certified aggregate barrier currently uses Git worktree staging. Read-only planner/research/risk-review teammates can still run concurrently in directory workspaces.

## Ephemeral workspaces

`medusa_runtime::workspace::Workspace::ephemeral()` creates a Medusa-owned temporary directory for tasks that should not start from an existing project: drafting documentation, synthesizing supplied research, preparing reports, or generating disposable artifacts. Cleanup is explicit; Medusa never deletes an arbitrary persistent directory through the ephemeral cleanup API.

Programmatic example:

```rust
use medusa_runtime::{RuntimeController, workspace::Workspace};

let workspace = Workspace::ephemeral()?;
let runtime = RuntimeController::start_workspace(&workspace);
// submit work and collect/save the desired artifacts
workspace.cleanup()?;
```

For CLI use, any ordinary directory can act as the same bounded workspace:

```bash
mkdir -p /tmp/medusa-report
medusa --repo /tmp/medusa-report --prompt "Create report.md from the supplied material"
```

The Desktop General Chat path is also backed by a Medusa-owned directory and therefore provides a user-facing non-Git workspace without asking the user to create a repository first. Explicit ephemeral-workspace creation remains a programmatic API; persistent user surfaces intentionally keep their normal durable session directory unless the user selects another workspace.

## Research, documentation, and general knowledge work

Git is not required for read-only or artifact-oriented work. Examples include:

- analyze files in a working directory and produce a report;
- create or revise documentation;
- synthesize attachments or workspace material;
- compare specifications or source sets;
- perform bounded multi-agent research over sources already available to Medusa;
- generate structured artifacts in an ordinary or ephemeral workspace.

A workspace mode does **not** grant new external capabilities. Browser actions remain unavailable to the model until the browser dispatcher and permission evidence are certified. Network research therefore depends on whichever explicitly supported, policy-authorized source or integration capabilities are available in the active Medusa build; a directory workspace alone does not create ambient network access.

## Safety invariants

Across every workspace mode:

- only the runtime coordinator may dispatch mutating implementers;
- nested autonomous delegation remains disabled;
- workers cannot widen their own write scope;
- worker output is evidence, not completion;
- the dedicated parent reviewer is zero-tool and cannot integrate;
- independent verification must bind to the exact immutable candidate and changed scope;
- integration authorization is separate from review and verification;
- primary-workspace drift invalidates prepared mutation rather than overwriting user changes;
- terminal success is reported only after the configured workspace verification authority accepts the result.

Schema field names such as `prepared_commit` and `prepared_tree` are retained for durable compatibility. In a directory workspace they hold content-addressed snapshot and tree identifiers rather than Git object IDs.
