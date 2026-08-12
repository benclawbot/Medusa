# Telegram remote frontend

Telegram is a transport and presentation adapter over the repository daemon. It never starts an
independent agent or reconstructs state from terminal output.

## Polling

```bash
export MEDUSA_TELEGRAM_BOT_TOKEN='123456:bot-secret'
medusa --repo /path/to/repository telegram \
  --allow-user 123456789
```

Private users and group users/chats use explicit numeric allowlists. Group messages require an
explicit bot mention unless `--no-require-mention` is set. Secrets are read only from named
environment variables and are never accepted as command-line values.

Use `--once` to perform one bounded polling/delivery cycle for deployment conformance without
starting the long-running supervisor.

## Webhook

Terminate TLS at a reverse proxy and forward only the configured path to the loopback listener:

```bash
export MEDUSA_TELEGRAM_BOT_TOKEN='123456:bot-secret'
export MEDUSA_TELEGRAM_WEBHOOK_SECRET='unguessable_secret'
medusa --repo /path/to/repository telegram \
  --allow-user 123456789 \
  --transport webhook \
  --webhook-public-url https://agent.example/telegram/update \
  --webhook-bind 127.0.0.1:8787 \
  --webhook-path /telegram/update
```

The local listener rejects non-loopback peers, requires Telegram's secret-token header, and bounds
request headers and bodies. Polling and webhook are mutually exclusive and use the same runtime.

## Voice notes

Voice is disabled by default. Enable it with `--voice voice-only` or `--voice all` and provide an
OpenAI audio credential through `OPENAI_API_KEY` (or another variable selected with
`--openai-token-env`). Transcription and synthesized OGG/Opus replies remain I/O over the same
Medusa session; runtime policy, approvals, containment, cancellation, and verification remain in the
daemon.

## Durable state

Telegram stores only transport state under `.medusa/telegram/`: update offsets, chat/topic bindings,
delivery cursors, Telegram message IDs, callbacks, media batches, and display/voice preferences.
Session, transcript, task, worker, approval, verification, and mutation truth remain in the daemon's
canonical journal and frontend control plane.
