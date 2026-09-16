# Provider support authority

This file is generated from `docs/provider-support.json`. The manifest is the reviewed support and live-dogfood authority; `medusa-config` tests keep the selectable Rust catalog synchronized with it.

| Provider | Support tier | Runtime protocol | Credential | Live dogfood |
|---|---|---|---|---|
| `minimax` | `production-supported` | `anthropic` | `MINIMAX_API_KEY` | `primary` |
| `anthropic` | `production-supported` | `anthropic` | `ANTHROPIC_API_KEY` | `configurable` |
| `anthropic-compatible` | `custom` | `anthropic` | `MEDUSA_API_KEY` | `not-enabled` |
| `openai` | `production-supported` | `openai` | `OPENAI_API_KEY` | `configurable` |
| `openai-oauth` | `production-supported` | `openai` | `external/local route` | `not-enabled` |
| `openai-compatible` | `custom` | `openai` | `MEDUSA_API_KEY` | `not-enabled` |
| `omniroute` | `managed` | `openai` | `external/local route` | `configurable` |
| `local` | `local` | `openai` | `external/local route` | `not-enabled` |

`production-supported` describes the selectable provider route. Custom, managed, and local routes retain operator-owned endpoint dependencies.

The scheduled cross-platform live dogfood gate resolves its provider, model, protocol, endpoint, authentication mode, and credential environment from the single `primary` entry. Other selectable routes remain configurable but are not represented as having passed that gate.

## Quarantined live evidence



See `docs/LIVE-PROVIDER-DOGFOOD.md` for the bounded evidence contract and `docs/PROVIDER-DELIVERY.md` for first-run diagnostics.
