# Observability

Medusa records structured operational events as JSON Lines under `.medusa/observability/events.jsonl`. Events carry timestamps, levels, components, event names, correlation IDs, and structured fields.

Credential-like keys and known token prefixes are redacted before persistence. Metrics use stable alphanumeric, underscore, and dot-separated names. Counters and latency samples can be exported through the `Observability::snapshot` API.

Recommended tracked metrics include task success, verification failures, retries, tool calls, rollback success, memory reuse, improvement regression, provider latency, and daemon reconnects.

Operational data is evidence, not semantic memory. It must not be injected into model context without the same validation and provenance controls used elsewhere.
