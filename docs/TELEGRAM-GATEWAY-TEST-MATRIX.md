# Telegram gateway network test matrix

The automated network suite exercises the production loopback transports rather than substituting transport mocks at the gateway boundary.

| Surface | Covered behavior |
| --- | --- |
| Mini App HTTP | Configurable path, launch-ticket rejection, Telegram `initData` authentication, distinct session bearer token, Realtime failure handling, transcript validation, bounded queue saturation, disconnected runtime, malformed and oversized requests, security headers, and shutdown |
| Webhook HTTP | Loopback binding, method and path rejection, secret-token authentication, body bounds, transfer-encoding rejection, typed update decoding, handler success/failure, and shutdown |
| OpenAI audio | Credential and endpoint validation, multipart transcription, transcript validation, authenticated JSON speech requests, OGG/Opus validation, and provider status classification |

These deterministic tests complement rather than replace the credential-gated Telegram/OpenAI acceptance evidence required for production closure.
