# Telegram gateway network test matrix

The automated network suite exercises the production loopback transports rather than substituting transport mocks at the gateway boundary. It uses only portable loopback TCP behavior so the daemon matrix validates the same contract on Linux, macOS, and Windows. Request assertions verify authenticated header structure without embedding credentials in failure output. The client retains complete response bytes when macOS reports a post-response connection reset instead of EOF.

| Surface | Covered behavior |
| --- | --- |
| Mini App HTTP | Configurable path, launch-ticket rejection, Telegram `initData` authentication, distinct session bearer token, Realtime failure handling, transcript validation, bounded queue saturation, disconnected runtime, malformed and oversized requests, security headers, and shutdown |
| Webhook HTTP | Loopback binding, method and path rejection, secret-token authentication, body bounds, transfer-encoding rejection, typed update decoding, handler success/failure, and shutdown |
| OpenAI audio | Credential and endpoint validation, multipart transcription, transcript validation, authenticated JSON speech requests, OGG/Opus validation, and provider status classification |
| Bot API client | Typed polling, bot and file metadata, chat actions, reactions, messages, idempotent edits, callbacks, bounded file downloads, retry-after handling, server failures, malformed envelopes, and credential redaction |
| Buffered prompts | Persisted text fragments and media groups submit through an already-acknowledged path that cannot regress the global transport cursor or a newer per-binding update cursor after an intervening update |

These deterministic tests complement rather than replace the credential-gated Telegram/OpenAI acceptance evidence required for production closure.
