# Telegram live-session gateway

The Telegram gateway is an authenticated frontend to Medusa's daemon-owned live-session control plane. It does not create a Telegram-specific agent, transcript, approval policy, or repository mutation path.

The checked-in implementation is ordinary Rust source and does not rely on build-time source mutation.

## Architecture

```text
Telegram update / callback / voice transcript / Mini App transcript
  -> Telegram transport authentication and bounded decoding
  -> TelegramSessionService
  -> FrontendControlPlane
  -> LiveSessionBroker
  -> RuntimeController
  -> typed frontend event replay
  -> deterministic Telegram renderer
```

Long-running work remains owned by the daemon when Telegram disconnects. TUI, desktop, Telegram, and Mini App interactions observe and control the same session.

## Credentials and authorization

Store the Bot API token outside repository configuration and construct `TelegramBotApiToken` at the process boundary. Tokens are redacted from `Debug` output and must never be written to daemon state or logs.

Configure explicit allowlists:

- Telegram user IDs allowed to submit or control sessions;
- Telegram chat IDs allowed to bind sessions;
- group policy requiring an explicit bot mention where appropriate;
- repository/profile restrictions enforced by the existing frontend control plane.

Callbacks are single-use, expiring, and bound to user, chat, topic, session, and turn. Mini App launch tickets are HMAC-signed, expire after five minutes, and are bound to the same identity and selected session.

## Polling mode

Polling mode is the local/default deployment mode.

1. Verify the bot identity with `getMe`.
2. Remove any configured webhook before polling.
3. Install the supported command menu with `setMyCommands`.
4. Start `TelegramPollingRuntime::run_until_cancelled` through `TelegramServiceSupervisor`.

The durable Bot API cursor advances only after an update has either been rejected safely or persisted/forwarded. Media groups and rapid text fragments are persisted before acknowledgement, so restarts cannot split or lose a logical prompt.

## Webhook mode

Webhook mode is intended for a reverse-proxied deployment.

- Terminate TLS at the external reverse proxy.
- Bind `TelegramWebhookServer` only to loopback.
- Use a public HTTPS URL with `setWebhook`.
- Configure a high-entropy `secret_token`; the receiver requires Telegram's `X-Telegram-Bot-Api-Secret-Token` header and compares it in constant time.
- Proxy only the configured webhook path to the loopback listener.
- Do not run polling and webhook mode simultaneously. `TelegramServiceSupervisor` enforces mutual exclusivity and removes the webhook on shutdown.

The receiver rejects non-loopback peers, non-POST requests, malformed transfer encoding, oversized headers, and request bodies over 1 MiB.

## Sessions and commands

Supported command surfaces include:

- `/sessions`
- `/new`
- `/attach <session>`
- `/detach`
- `/resume <session>`
- `/status`
- `/stop`
- `/toolprogress off|new|all|verbose`
- `/voice off|on|tts|status`
- `/help`

Busy submissions use the shared queue semantics. `/stop`, approvals, answers, model/configuration changes, and supported steering actions dispatch through the same frontend protocol as desktop and TUI.

## Rendering and delivery

The renderer consumes typed presentation events only. It provides:

- processing/success/failure reactions;
- refreshed typing actions;
- throttled edit-in-place previews;
- UTF-16-aware chunking and continuation replies;
- Telegram-safe MarkdownV2 with plain-text fallback;
- one long-running heartbeat bubble;
- deterministic plan, question, approval, team, verification, settings, notice, and failure mappings;
- replay-safe message slots and event cursors;
- bounded retry handling for flood control and transient transport errors.

Native artifacts are resolved by opaque ID through the daemon artifact store. The store verifies the content digest and returns bounded bytes, display name, and MIME type. Telegram never receives or resolves arbitrary repository paths.

## Inbound media and batching

Photos, supported text documents, voice notes, and audio files use bounded Bot API downloads. Filenames are traversal-safe and only opaque artifact IDs cross the frontend protocol.

- Albums are persisted and coalesced across long-poll responses and daemon restarts.
- Rapid plain-text fragments are persisted and coalesced after a 600 ms quiet period.
- A later message from the same chat/topic/user flushes pending content first to preserve order.
- Redelivered album members and text fragments are deduplicated by Telegram message ID.

## Voice notes and native voice replies

`TelegramVoicePipeline` uses authenticated OpenAI audio endpoints:

- incoming voice/audio: bounded download -> `/v1/audio/transcriptions` -> canonical text submission to the current Medusa session;
- outgoing speech: final canonical assistant text -> `/v1/audio/speech` with Opus -> validated OGG stream -> native Telegram voice bubble.

Text remains the canonical accessible response. Voice modes:

- `off`: text only;
- `voice_only`: synthesize only when the current turn originated from Telegram voice/audio;
- `all`: synthesize every final assistant response.

The pending voice-reply marker is durable and command-aware, so a queued voice prompt cannot cause an unrelated active text turn to speak. It is cleared only after the corresponding final voice bubble is accepted. Synthesis and `sendVoice` occur before the presentation cursor advances.

## Mini App duplex voice

A normal Bot API chat cannot provide a continuous bidirectional call stream. The Mini App supplies true duplex voice while preserving the authoritative Medusa session.

1. The renderer creates an inline Web App button with a signed launch ticket bound to the current chat/topic/user/session.
2. `TelegramMiniAppHttpServer` serves the client on loopback behind HTTPS reverse proxying.
3. `/auth` verifies the launch ticket and Telegram `initData` HMAC and freshness.
4. `/realtime` mints the short-lived OpenAI WebRTC credential used by the authenticated Telegram Mini App route.
5. The browser captures microphone audio and plays assistant audio through WebRTC.
6. Final user transcripts are submitted to `/transcript` and placed on a bounded channel.
7. The polling/runtime owner drains that channel into `TelegramSessionService`; no second runtime owner or competing repository mutation path is created.

After `/auth`, protected Mini App endpoints require a distinct authenticated session token rather than reusing the launch ticket. The client derives endpoint paths from its configured base URL, tears down media and peer-connection state deterministically, and exposes only sanitized duplex evidence.

Mini App endpoints return `Cache-Control: no-store`, validate bounded JSON, require bearer session tokens after authentication, and expose no long-lived OpenAI credential.

## Durable state

Default transport state locations:

- service bindings/cursors/callbacks: configured daemon Telegram state path;
- media groups: `.medusa/telegram-media-groups.json` or `MEDUSA_TELEGRAM_MEDIA_GROUP_STATE_PATH`;
- text fragments: `.medusa/telegram-text-fragments.json` or `MEDUSA_TELEGRAM_TEXT_FRAGMENT_STATE_PATH`.

State files use explicit schema versions, reject unknown fields, enforce count and byte bounds, and are written through a synchronized temporary file followed by rename.

## Operations and troubleshooting

### Bot receives no updates

- Polling: confirm any old webhook was removed and the bot token passes `getMe`.
- Webhook: inspect `getWebhookInfo`, reverse-proxy routing, public HTTPS certificate, and secret-token forwarding.
- Confirm user/chat allowlists and group mention policy.

### Duplicate or missing prompts

Inspect the durable update offset and pending media/text state. Do not manually advance the Bot API cursor. Corrupt state fails closed rather than being silently discarded.

### Markdown delivery fails

The gateway retries the final message as plain text. Repeated transport failures remain pending and do not advance the event cursor.

### Voice note fails

Check the bounded Bot API download, supported audio MIME type, OpenAI audio credential, transcription model, and provider availability. Invalid user audio is rejected safely; transient provider errors are retried by the supervised runtime.

### Mini App cannot connect

Check the public HTTPS URL, launch-ticket expiry, Telegram `initData` age, reverse-proxy paths, browser microphone permission, authenticated OpenAI Realtime capability, and WebRTC network policy.

## Live acceptance evidence

Production closure requires credential-gated evidence for:

- Bot API identity and command installation;
- polling or webhook update ingestion with cursor persistence;
- same-session text, callback, approval/question, media, album, and rapid-fragment flows;
- Telegram voice-note transcription into the current session;
- final text plus native OGG/Opus voice-bubble delivery;
- Telegram Mini App `initData` verification, signed session binding, WebRTC credential establishment, microphone transcript, assistant audio, and same-session event replay;
- cancellation, reconnect/restart replay, flood control, and secret redaction.

Deterministic tests and mock transport evidence are necessary but do not replace the credential-gated Bot API/OpenAI slice.
