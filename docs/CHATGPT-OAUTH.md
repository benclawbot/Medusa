# ChatGPT OAuth gateway

Medusa's ChatGPT OAuth route depends on the external `openai-oauth` loopback gateway. Medusa does not read, copy, or store the user's private OAuth credential files.

Before an interactive, `run`, or `resume` coding session starts, Medusa verifies the configured gateway at the profile `base_url` (default `http://127.0.0.1:10531/v1`). The preflight runs once at process startup, before the coding runtime accepts work, and:

1. calls `GET /models` and requires the configured model to be present;
2. submits a forced function call and requires OpenAI-compatible `tool_calls`;
3. submits a streaming request and requires valid SSE `data:` events plus a `[DONE]` terminator;
4. classifies authentication, gateway reachability, missing model, and protocol failures separately.

The gateway does not expose a portable cancellation-capability endpoint, so cancellation compatibility is reported explicitly as **unverified** rather than implied. Preflight results are startup evidence only; per-request runtime error handling remains authoritative.

## Offline startup

Set `MEDUSA_OAUTH_PREFLIGHT=off` to skip network verification. Medusa prints an explicit warning that model, tool-calling, streaming, and cancellation compatibility are unverified. This is intended for offline startup and must not be treated as a successful capability check.

## Authentication safety

A real end-to-end OAuth test requires a user-authorized local gateway. Private OAuth credentials must not be copied into repository secrets. Automated contract tests exercise response parsing and failure classification without user credentials.
