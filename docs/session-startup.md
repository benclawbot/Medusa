# Session startup semantics

Medusa starts a fresh in-memory session unless the user explicitly requests durable state restoration.

- `medusa` starts fresh.
- `medusa --resume <session-id>` restores that exact repository session.
- `medusa --continue` restores the most recently updated durable session for the repository.

Application restarts and updater relaunches do not implicitly resume a prior session. A missing or repository-mismatched session fails closed instead of presenting stale work as active.
