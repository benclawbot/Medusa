# Team Observability and Steering

This slice exposes the production multi-agent coordinator through one typed runtime control plane.

## Required behavior

- Every coordinated worker has a stable ID, role, task, lifecycle state, session ID, turn count, and last update.
- Runtime events publish complete team snapshots after every lifecycle transition.
- `/team` displays the current snapshot even while an agent turn is running.
- `/steer <worker> <instruction>` queues a bounded instruction that the selected worker consumes between model turns.
- `/stop-worker <worker>` cancels one worker without cancelling unrelated teammates.
- `/stop-team` requests graceful shutdown for the coordinated team and fails the current coordinated turn closed.
- Commands are applied through shared runtime state rather than the blocked product command loop.
- Direct single-agent mode has no team overhead.
- TUI and desktop adapters preserve typed status and render useful worker progress.

This document is the implementation contract for issue #550 and will be updated to shipped behavior before merge.
