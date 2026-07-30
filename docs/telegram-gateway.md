# Telegram live-session gateway

This document defines the production boundary introduced for issue #568. The Telegram integration is a frontend to the same authoritative Medusa runtime and durable session state used by the terminal, desktop, and headless clients. It is not a second agent runtime.

## Ownership

`medusa-protocol::frontend` owns the versioned, serializable frontend command and presentation event contract. Commands carry stable command and idempotency identifiers. Events carry a monotonically increasing durable cursor, session and turn identity, parent and correlation identity, lifecycle, and typed presentation data.

`medusa-daemon::telegram` owns Telegram-specific authorization, command translation, callback safety, and deterministic render actions. A transport adapter may execute those actions through the Telegram Bot API, but it must not infer runtime state from message text or tool titles.

The live-session broker remains authoritative for session creation, attachment, replay, cancellation, questions, approvals, model changes, effort, plan mode, worker controls, and execution policy.

## Security defaults

Telegram is disabled by default. Enabling it without at least one numeric user allowlist entry is invalid.

Authorization uses immutable numeric Telegram user, chat, and topic identifiers. Usernames are not authorization inputs. Private chats require `allowed_users`. Groups and supergroups require both an allowed chat and an allowed group user; explicit bot mention is required by default. Channels are denied.

Webhook mode requires a configured webhook secret before startup. Bot tokens, webhook secrets, Mini App signing secrets, and provider credentials are not represented in the renderer or presentation event contract.

Approval buttons use opaque, bounded callback values. Server-side records bind each callback to the user, chat, topic, session, turn, approval, decision, and expiry. Callbacks are one-shot and replay-safe. The gateway forwards the decision to the authoritative runtime; it never performs an approved action itself.

## Command mapping

The initial mapper supports:

- `/new [objective]`
- `/sessions`
- `/attach <session>`
- `/detach`
- `/resume <session>`
- `/status`
- `/stop`
- `/model <model>`
- `/effort <low|medium|high>`
- `/plan <on|off>`
- `/verbose <off|new|all|verbose>`
- `/voice <off|on|tts|status|live>`
- `/help`

Ordinary text becomes a shared `Submit` command. Telegram message identity produces a stable idempotency key, so webhook retries or polling redelivery do not create duplicate runtime commands.

## Rendering contract

The renderer consumes only typed `FrontendEventEnvelope` values and emits abstract `TelegramAction` values. Stable message slots allow the transport to edit in place while persisting Telegram message identifiers separately.

The initial action model covers source-message reactions, typing state, progressive assistant previews, progress cards, plan and team cards, questions, approvals, notices, artifacts, cancellation, failure, and completion. Activity icons are selected from the typed activity kind rather than title matching.

Streaming previews are plain text and include a configurable cursor. Final assistant output uses conservative MarkdownV2 escaping. Message splitting uses Telegram's UTF-16 code-unit limits, does not cut Unicode scalar values, and closes and reopens fenced code blocks where possible. Basic Markdown tables are normalized into compact row bullets before escaping.

Each renderer records cursor-to-event identity. Replaying the same event is a no-op. Reusing a cursor for a different event or delivering an older unknown cursor is rejected rather than rendered out of order.

## Remaining transport work

This foundation intentionally leaves network I/O, durable chat/topic binding, replay from the session journal, media-group batching, Bot API retry scheduling, webhook/polling lifecycle, voice transcription and synthesis, and the signed duplex Mini App transport to follow-up commits on issue #568. Those components must consume these contracts rather than bypass them.
