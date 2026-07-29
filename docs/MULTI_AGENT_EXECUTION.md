# Multi-Agent Execution Boundary

Medusa uses one product lifecycle owner, one team coordinator, and one single-agent execution engine.

- `RuntimeController` owns the user-facing session and cancellation lifecycle.
- The production multi-agent coordinator owns read-only preflight task state, leases, worker lifecycle, messaging, evidence integration, cancellation, and completion gating.
- `AgentEngine` executes one model-backed agent session. It does not own the team task graph.
- Role-bound execution policy is enforced by runtime code. Prompts are not an authorization boundary.
- Team membership and mailboxes are durable and validated when restored.
- Read-only planner and risk-reviewer teammates are dispatched in production. Mutating teammates remain disabled until worktree isolation and guarded integration are enabled.
- The parent coordinator remains responsible for accepting results and passing repository verification before success is reported.

Components that duplicate these responsibilities must be consolidated or removed after their replacement path has behavioral and recovery coverage. See issue #550 for the refactor ledger and production acceptance checklist.

## Current production slice

Coordinated prompts dispatch independent planner and risk-reviewer sessions in parallel. Each teammate receives a role-bound read-only policy, a durable lease, a separate session, and a repository-snapshot-bound context packet. Their validated evidence is added to protected parent context. The parent is the only mutation authority and cannot report completed coordinated work until repository verification passes.
