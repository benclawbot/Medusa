# Multi-Agent Execution Boundary

Medusa uses one product lifecycle owner, coordinated task state, and independent `AgentEngine` sessions with runtime-enforced roles.

- `RuntimeController` owns the user-facing session and cancellation lifecycle.
- `multi_agent_coordinator` owns read-only preflight task state, leases, worker lifecycle, messaging, evidence integration, and workspace-snapshot binding.
- `mutating_worker_coordinator` owns isolated implementer execution, scope validation, candidate verification, deterministic candidate preparation, guarded integration, rollback, and cleanup.
- `parallel_mutation` and `parallel_mutation_batch` own conflict-aware Git mutation decomposition, bounded concurrent waves, child acceptance, deterministic aggregate staging, and aggregate verification.
- `AgentEngine` executes one model-backed session. It does not own the team task graph or integration policy.
- Role-bound execution policy is enforced by runtime code. Prompts are not an authorization boundary.
- Team membership, mailboxes, lease epochs, prepared candidates, changed paths, verification evidence, and integration receipts are durable.
- The parent session is a read-only lead and reviewer for coordinated objectives and cannot bypass mutation isolation with direct mutation tools.
- The final workspace verification gate remains mandatory before success is reported.

Components that duplicate these responsibilities must be consolidated or removed after their replacement path has behavioral and recovery coverage. See issue #550 for the refactor ledger and production acceptance checklist.

## Current production slice

Coordinated prompts dispatch planner and risk-reviewer sessions in parallel. Long read-only analysis, documentation, and research objectives may stop there and create no mutating worker.

Explicit mutation objectives select a workspace backend:

- **Git workspace.** A safe implementation scope may decompose into a conflict-aware mutation DAG with up to three concurrent implementers. Every child owns an exact file/resource contract and runs in its own Git worktree. High-risk, ambiguous, repository-wide, directory-wide, oversized, low-confidence, or resource-conflicting plans fall back to one implementer.
- **Directory or ephemeral workspace.** One implementer receives an isolated content-addressed snapshot. Git is not required. Primary-workspace drift is detected before integration and authorized path replacement is rollback-protected.

For Git parallel mutation, conflict-free tasks run in bounded waves. Each child is scope-checked, verified, and independently accepted through the mutation transaction authority. An `IntegrationBarrier` binds the accepted child evidence to the DAG and dependency order. A separate aggregate worktree deterministically composes child commits, validates the union scope, verifies the aggregate, and prepares one immutable aggregate transaction for final parent review and integration. Child workers cannot integrate themselves and a successful child cannot bypass aggregate verification.

Durable coordinator records live under `.medusa/executions/<execution-id>`. The execution identity binds the task plan to a deterministic workspace-content fingerprint. Existing partial Git worktrees may be resumed only when their branch and base still match the primary repository. Directory candidates use immutable baseline and snapshot manifests; primary drift invalidates integration instead of overwriting newer user work.

## Delegation boundary

The shipped parallel implementation path is **bounded orchestration**, not autonomous recursion:

- only the root runtime/coordinator may create workers;
- implementers cannot spawn additional implementers;
- no worker may widen its own write scope;
- parent review and integration authority stay centralized;
- consensus voting and model-driven unconstrained team expansion are not production capabilities;
- distributed multi-host/multi-process mutation transactions remain out of scope.

`agent.parallel_workers` is not an autonomous-agent-team size knob. It controls bounded tool-level parallel work in configuration schema v1. The conflict-aware Git mutation DAG has its own safety bound (currently three mutating children) and only activates when the typed decomposition passes its risk, confidence, scope, and resource-conflict rules.

## Workspace independence

Read-only teammate coordination and general artifact work do not require Git. Medusa can use an ordinary directory for documentation, analysis, supplied-source research, reports, and other bounded work. Programmatic clients can also create an explicit ephemeral workspace. See [Workspace modes](WORKSPACES.md).

A non-Git workspace does not add ambient network or browser authority. External research depends on separately supported and authorized source/integration capabilities. Model-executable browser actions remain quarantined until their dedicated production evidence is complete.

## Production evidence

Git-backed multi-implementer execution is covered by the cross-platform **Parallel Mutation Certification** gate. Its acceptance suite exercises DAG behavior, production runtime wiring, deterministic integration, rollback and cleanup, scope/fallback invalidation, and performance evidence. Directory workspace isolation has separate cross-platform tests for content-addressed candidate preparation, independent materialization, authorized integration, cleanup, and primary-drift rejection.

## Remaining boundary

Autonomous nested delegation, unconstrained dynamic team expansion, consensus voting, distributed multi-worker transactions, and non-Git parallel mutation remain outside the production entrypoint. Non-Git parallel mutation requires a separately certified aggregate transaction backend before it can replace the current safe single-snapshot implementer path.
