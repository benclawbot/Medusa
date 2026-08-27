# ChatGPT OAuth

Medusa's ChatGPT OAuth route uses the installed Codex CLI app-server over stdio. Codex owns browser login, token refresh, credential storage, account state, and upstream transport; Medusa does not read, copy, or store private OAuth credential files.

## Setup

Run `medusa config` and choose **ChatGPT OAuth via Codex app-server**. Medusa starts `codex app-server --stdio` when it needs account setup or a model turn. If Codex is not installed or is not on `PATH`, install the Codex CLI and retry.

Before an interactive, `run`, or `resume` coding session starts, Medusa performs a bounded Codex app-server account/model preflight. The default fast preflight checks the authenticated ChatGPT account and configured model. For a full compatibility check, set `MEDUSA_OAUTH_PREFLIGHT=full`; live turn behavior is then exercised by the first actual request and:

1. calls the app-server account and model methods and requires the configured model to be present;
2. classifies authentication, Codex executable availability, missing model, and protocol failures separately.

Preflight results are startup evidence only; per-request app-server error, approval, and cancellation handling remains authoritative.

## Offline startup

Set `MEDUSA_OAUTH_PREFLIGHT=off` to skip account/model verification. Medusa prints an explicit warning that authentication and model compatibility will be checked on the first turn. This is intended for offline startup and must not be treated as a successful capability check.

## Authentication safety

A real end-to-end OAuth test requires a user-authorized Codex account. Private OAuth credentials must not be copied into repository secrets. Automated contract tests exercise app-server protocol parsing and failure classification without user credentials.

## Desktop voice status

ChatGPT OAuth is text-only in the desktop application. The Codex app-server provides the authenticated coding/chat route, but it does not provide the Realtime session credential required by the desktop microphone/WebRTC transport.

The desktop voice button, Realtime WebRTC transport, and live-evidence screen have therefore been removed. Medusa does not show or store a separate voice API-key field, and selecting ChatGPT OAuth cannot start desktop voice. Realtime voice surfaces in the TUI and Telegram gateway are separate capabilities with their own provider and live-acceptance requirements.

## OpenAI API alternative

The separate `openai-api` connection uses `https://api.openai.com/v1` with `OPENAI_API_KEY`. MiniMax, Anthropic, OmniRoute, local runtimes, and compatible custom endpoints remain separately selectable according to `docs/provider-support.json`.
