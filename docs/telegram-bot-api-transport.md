# Telegram Bot API transport

The Telegram gateway uses the daemon-owned blocking Bot API client under
`medusa_daemon::telegram::bot_api`. The client is a transport boundary only: it does not own an
agent, transcript, approval policy, or runtime session.

## Security boundary

- Bot tokens are validated at construction and redacted from `Debug` output and API errors.
- Production endpoints must use HTTPS. Plain HTTP is accepted only for loopback test servers.
- Response bodies are bounded before JSON decoding.
- Telegram rejection descriptions are length-bounded and token-redacted.

## Polling and delivery

Long polling accepts only message and callback-query updates. `TelegramUpdateCursor` advances to
`update_id + 1` only after the caller acknowledges an update, and rejects cursor regression. This
keeps replay and retry decisions with the daemon-owned gateway service rather than the Bot API DTO
layer. The cursor is exported as part of the transport API so daemon-owned polling code can retain
and advance acknowledgement state explicitly.

Flood-control responses are represented as `RetryAfter`; timeouts, connection failures, read
failures, and server failures are explicitly classified as transient. A Telegram
`message is not modified` response is treated as a successful no-op edit.

Outbound Bot API request models support round-trip deserialization in tests while preserving the
exact Telegram wire spellings used for parse modes and update payloads.

## Runtime ownership

Decoded updates must be mapped through the existing Telegram authorization and frontend-command
path. Presentation events must continue through the replay-safe Telegram renderer. The transport
must never instantiate a separate Medusa agent or infer authoritative state from Telegram messages.
