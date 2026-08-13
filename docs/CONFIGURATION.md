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
browser_on_ui_change = true
```

Configuration precedence is CLI overrides, environment overrides, project TOML, user TOML, then built-in defaults.

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

## Browser verification policy

When `verification.browser_on_ui_change` is enabled, effective UI changes automatically require browser verification. Documentation-only, generated, snapshot-only, lockfile, and build-output changes are skipped. Set `MEDUSA_BROWSER_VERIFY=force` or `MEDUSA_BROWSER_VERIFY=skip` for an explicit audited override. A runnable route must be supplied through `MEDUSA_BROWSER_VERIFY_URL`; `MEDUSA_BROWSERD` may override the browser daemon executable. Evidence records the override, tested route, HTTP status, snapshot assertions, screenshot path, console errors, and final browser result.

Model-executable browser actions additionally require the explicitly enabled `MEDUSA_BROWSER_PATH` sidecar. `MEDUSA_BROWSER_TIMEOUT_MS` is a **wall-clock deadline for each browser request**, including the initial verification-route navigation; it is not merely an idle-cleanup delay. A zero value is treated as a 1 ms minimum deadline. In-flight runtime cancellation terminates and resets the affected sidecar, and timeout/protocol failures also discard that sidecar so a late response cannot be consumed by a later request. After a successful request, one resettable lifecycle worker per browser session uses the same configured interval for idle cleanup rather than spawning one sleeping worker per action.

Browser protocol framing is bounded before JSON parsing: request frames are limited to 64 KiB and response frames to 12 MiB. The Playwright bridge also rejects snapshots above 4,096 element references or 1 MiB of text, evaluation results above 1 MiB, screenshots above 16 Mi pixels or 8 MiB encoded image bytes, and any aggregate response above the response-frame ceiling. Limit violations fail deterministically instead of returning partial unlabelled data.
