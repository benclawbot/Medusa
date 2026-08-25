# ChatGPT OAuth

Medusa's ChatGPT OAuth route depends on the external `openai-oauth` loopback gateway. Medusa does not read, copy, or store the user's private OAuth credential files.

## Setup

Run `medusa config` and choose **ChatGPT OAuth via local openai-oauth gateway**. The gateway owns browser login, token refresh, credential storage, and account-aware model access. If the loopback gateway is not running, Medusa starts the pinned gateway with `npx --yes openai-oauth@2.0.0 --no-open --detach`.

The gateway can also be managed directly with `npx openai-oauth@2.0.0 login`, `npx openai-oauth@2.0.0 status`, and `npx openai-oauth@2.0.0 stop`. Keep it bound to loopback; port `10531` must not be exposed to a LAN or the internet.

Before an interactive, `run`, or `resume` coding session starts, Medusa verifies the configured gateway at the profile `base_url` (default `http://127.0.0.1:10531/v1`). The default fast preflight performs one bounded `GET /models` check so an already-authenticated gateway can accept work with minimal startup latency. For a full compatibility check, set `MEDUSA_OAUTH_PREFLIGHT=full`; that runs once at process startup and:

1. calls `GET /models` and requires the configured model to be present;
2. submits a forced function call and requires OpenAI-compatible `tool_calls`;
3. submits a streaming request and requires valid SSE `data:` events plus a `[DONE]` terminator;
4. classifies authentication, gateway reachability, missing model, and protocol failures separately.

The gateway does not expose a portable cancellation-capability endpoint, so cancellation compatibility is reported explicitly as **unverified** rather than implied. Preflight results are startup evidence only; per-request runtime error handling remains authoritative.

## Offline startup

Set `MEDUSA_OAUTH_PREFLIGHT=off` to skip network verification. Medusa prints an explicit warning that model, tool-calling, streaming, and cancellation compatibility are unverified. This is intended for offline startup and must not be treated as a successful capability check. The fast default defers tool-calling and streaming probes; use `full` when those capability checks are required.

## Authentication safety

A real end-to-end OAuth test requires a user-authorized local gateway. Private OAuth credentials must not be copied into repository secrets. Automated contract tests exercise response parsing and failure classification without user credentials.

## Desktop voice status

ChatGPT OAuth is text-only in the desktop application. The local `openai-oauth` gateway provides the authenticated REST route used by coding and chat sessions, but it does not provide the Realtime session credential required by the desktop microphone/WebRTC transport.

The desktop voice button, Realtime WebRTC transport, and live-evidence screen have therefore been removed. Medusa does not show or store a separate voice API-key field, and selecting ChatGPT OAuth cannot start desktop voice. Realtime voice surfaces in the TUI and Telegram gateway are separate capabilities with their own provider and live-acceptance requirements.

## OpenAI API alternative

The separate `openai-api` connection uses `https://api.openai.com/v1` with `OPENAI_API_KEY`. MiniMax, Anthropic, OmniRoute, local runtimes, and compatible custom endpoints remain separately selectable according to `docs/provider-support.json`.
