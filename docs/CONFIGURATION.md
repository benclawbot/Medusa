# Configuration

Medusa's typed TOML configuration accepts only fields that have an observable production effect. Unknown or removed fields fail during startup instead of being silently ignored.

## Supported schema and defaults

```toml
version = 1

[agent]
mode = "yolo"
max_turns = 500
parallel_workers = 4

[model]
provider = "minimax"
fallback_providers = []
# Optional role/phase pins to the existing route ids. Empty keeps the single-route default.
role_routes = {}
name = "MiniMax-M3"
protocol = "openai"
temperature_milli = 200
max_output_tokens = 32768
context_window_tokens = 1000000
auto_compact_percent = 40
auth = "api-key"
# base_url is optional

[memory]
enabled = true
format = "markdown"

[verification]
required = true
```

Configuration precedence is CLI overrides, environment overrides, project TOML, user TOML, then built-in defaults.

## Versioned runtime-loop configuration

Tunable loop behavior has a separate, closed schema so it cannot silently broaden the fixed
execution authorities. The optional user file is `runtime.toml` beside the provider profile, and
the optional repository file is `.medusa/runtime.toml`. Resolution is deterministic:
explicit built-in values, user runtime policy, repository runtime policy, bounded learned policy,
then an active session override. Missing files use the schema defaults; unknown fields, unsupported
schema versions, invalid budget relationships, unadmitted provider/model routes, unavailable Code
Mode, and unregistered service providers fail before a provider or tool is started.

Example repository policy:

```toml
schema_version = 1
tool_presentation = "native"
retry_budget = 2
replan_budget = 2
timeout_millis = 120000
compaction_threshold_tokens = 100000
model_output_chars = 262144
diagnostics_enabled = false
```

Provider/model selection may only name the configured primary route or an existing fallback route;
it does not install providers or change policy, approval, containment, verification, integration,
or journal authority. The effective configuration is frozen when a session starts. Its redacted
snapshot, schema version, provenance, and execution fingerprint are persisted with effective
request evidence, and a resumed session fails closed if the current runtime would compile a
different fingerprint. `/config explain` reports the same identity and which fields participate in
the execution fingerprint; diagnostic-only settings are explicitly excluded.

`model.role_routes` lets a user pin a role to `primary` or an existing `fallback[index]` route without
creating a second provider router. Supported role aliases include `planner`/`planning`,
`implementer`/`implementation`, `reviewer`/`high_risk_review`, `debugger`/`repair`,
`summarizer`/`summarization`, and `formatter`/`formatting`. A pinned route is attempted first for
that phase; the normal authorized failover routes remain available if it fails. Unknown roles and
missing fallback indexes are rejected during configuration validation.

`agent.parallel_workers` is retained for version-1 compatibility and currently controls bounded parallel tool work. It does not create additional independent coding agents in the current production runtime.

## Migration from ignored fields

The following version-1 keys were removed because they were validated and exposed publicly but had no authoritative production behavior:

- `agent.ask_policy`
- `model.speed`
- `model.reasoning`
- the entire `[runtime]` table (`backend`, `network`, and `process_limit`)
- the entire `[git]` table (`auto_commit`, `protect_dirty_tree`, and `allow_force_push`)
- `memory.auto_promote_low_risk`
- `verification.independent_review`

Delete these keys from user and project configuration files. Medusa now reports them as unknown fields with their TOML location, preventing a configuration file from promising behavior the runtime does not implement.

Provider-profile `speed` and `reasoning` values remain readable for compatibility with the provider-settings file, but they are not part of the public runtime TOML schema and are not projected into runtime configuration.

## UI verification policy

Effective UI changes require a bounded static HTTP smoke check. The verifier serves the generated
`index.html` from the repository's configured output, confirms a successful response and non-empty
document, and checks common accessibility failures such as images without `alt` text or unlabeled
form controls. The result is recorded as ordinary verification evidence; no browser daemon, Node.js,
Playwright installation, or environment-specific route is required.

This keeps UI verification deterministic and portable while leaving interactive browser inspection to
frontend development tools and release certification when those are explicitly needed.
