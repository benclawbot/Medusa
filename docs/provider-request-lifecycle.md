# Provider request lifecycle

Medusa surfaces each model request as an explicit runtime activity instead of leaving the UI at a generic waiting state.

- The activity names the configured provider and model.
- The activity states the 120-second per-attempt HTTP timeout and that retry or failover is bounded by route policy.
- Pressing `Esc` requests cancellation while the runtime is busy.
- A successful response updates the same activity with its elapsed duration.
- A timeout or provider failure is followed by the existing runtime failure event, which returns the composer to usable input.

This makes a slow provider distinguishable from tool execution and prevents an apparently unbounded, zero-token wait from being presented without diagnostics.
