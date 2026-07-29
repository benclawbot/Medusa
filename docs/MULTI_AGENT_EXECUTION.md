# Multi-Agent Execution Boundary

Medusa uses one product lifecycle owner, one team coordinator, and one single-agent execution engine.

- `RuntimeController` owns the user-facing session and cancellation lifecycle.
- The production multi-agent coordinator owns task state, leases, worker lifecycle, messaging, review, integration, and completion.
- `AgentEngine` executes one model-backed agent session. It does not own the team task graph.
- Role-bound execution policy is enforced by runtime code. Prompts are not an authorization boundary.
- Team membership and mailboxes are durable and validated when restored.
- Mutating workers must be isolated before production dispatch is enabled.
- The parent coordinator remains responsible for accepting results and passing repository verification before success is reported.

Components that duplicate these responsibilities must be consolidated or removed after their replacement path has behavioral and recovery coverage. See issue #550 for the refactor ledger and production acceptance checklist.
