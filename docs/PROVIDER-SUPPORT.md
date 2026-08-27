# Provider support authority

This file is generated from `docs/provider-support.json`. The manifest is the reviewed support and live-dogfood authority; `medusa-config` tests keep the selectable Rust catalog synchronized with it.

| Provider | Support tier | Runtime protocol | Credential | Live dogfood | Realtime voice |
|---|---|---|---|---|---|
| `minimax` | `production-supported` | `anthropic` | `MINIMAX_API_KEY` | `primary` | `unavailable` |
| `anthropic` | `production-supported` | `anthropic` | `ANTHROPIC_API_KEY` | `configurable` | `unavailable` |
| `anthropic-compatible` | `custom` | `anthropic` | `MEDUSA_API_KEY` | `not-enabled` | `unavailable` |
| `openai` | `production-supported` | `openai` | `OPENAI_API_KEY` | `configurable` | `unavailable` |
| `openai-oauth` | `production-supported` | `openai` | `external/local route` | `not-enabled` | `unavailable` |
| `openai-compatible` | `custom` | `openai` | `MEDUSA_API_KEY` | `not-enabled` | `unavailable` |
| `omniroute` | `managed` | `openai` | `external/local route` | `configurable` | `unavailable` |
| `local` | `local` | `openai` | `external/local route` | `not-enabled` | `unavailable` |

`production-supported` describes the selectable text/provider route; it does not promote a separate realtime or remote-frontend capability. Custom, managed, and local routes retain operator-owned endpoint dependencies.

The scheduled cross-platform live dogfood gate resolves its provider, model, protocol, endpoint, authentication mode, and credential environment from the single `primary` entry. Other selectable routes remain configurable but are not represented as having passed that gate.

## Quarantined live evidence

- `telegram-live-evidence`: Repository tests are complete; real bot, chat, Mini App, and sanitized live evidence are still required.

The desktop application does not expose Realtime voice. ChatGPT OAuth remains a text-provider route because the Codex app-server does not provide the Realtime session credential required by a desktop microphone/WebRTC client.

See `docs/LIVE-PROVIDER-DOGFOOD.md` for the bounded evidence contract and `docs/PROVIDER-DELIVERY.md` for first-run diagnostics.
