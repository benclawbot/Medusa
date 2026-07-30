# Multi-Agent Execution Boundary

Medusa uses one product lifecycle owner, coordinated task state, and independent `AgentEngine` sessions with runtime-enforced roles.

- `RuntimeController` owns the user-facing session and cancellation lifecycle.
- `multi_agent_coordinator` owns read-only preflight task state, leases, worker lifecycle, messaging, evidence integration, and repository-snapshot binding.
- `mutating_worker_coordinator` owns worktree-isolated implementer execution, scope validation, worktree verification, deterministic commit preparation, guarded integration, rollback, and cleanup.
- `AgentEngine` executes one model-backed session. It does not own the team task graph or integration policy.
- Role-bound execution policy is enforced by runtime code. Prompts are not an authorization boundary.
- Team membership, mailboxes, lease epochs, prepared commits, changed paths, verification evidence, and integration receipts are durable.
- The parent session is a read-only lead and reviewer for coordinated objectives and cannot bypass worktree isolation with direct mutation tools.
- The final primary repository verification gate remains mandatory before success is reported.

Components that duplicate these responsibilities must be consolidated or removed after their replacement path has behavioral and recovery coverage. See issue #550 for the refactor ledger and production acceptance checklist.

## Current production slice

Coordinated prompts dispatch planner and risk-reviewer sessions in parallel. Long read-only analysis objectives stop there and create no mutating worktree. Explicit mutation objectives additionally dispatch one implementer contract in an execution-specific Git worktree. The coordinator removes temporary session state, checks changed paths against the contract, verifies the isolated worktree, creates one deterministic commit, and integrates it into a clean primary repository. Overlapping worker paths are rejected before integration and a conflict rolls the integration batch back to its original HEAD.

Durable coordinator records live under `.medusa/executions/<execution-id>`. The execution identity binds the task plan to a deterministic repository-content fingerprint. Existing partial worktrees may be resumed only when their branch and base still match the primary repository; prepared commits survive crashes through ancestry and tree-identity recovery.

## Remaining boundary

The active production path supports one mutating implementer contract. Autonomous nested delegation, dynamic multi-implementer decomposition, consensus voting, commit barriers, and distributed transaction coordination remain separate promotion work.
