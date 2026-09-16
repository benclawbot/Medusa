# Capability registry

`medusa-capabilities` owns one versioned capability snapshot shared by the model, CLI, TUI, desktop, protocol, and documentation projections. It reports availability and permissions; it does not execute external repository services.

Inspect the snapshot for a workspace:

```bash
medusa-capabilities /path/to/workspace
```

Every advertised capability must have a dispatcher, permission contract, focused tests, an owner, observability, and recovery semantics. Preview capabilities remain opt-in and fail closed when readiness or authorization is missing.
