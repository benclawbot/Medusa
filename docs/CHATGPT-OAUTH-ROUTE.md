# ChatGPT OAuth route

Medusa can use ChatGPT/Codex through the local `openai-oauth` gateway without API access or manual copy/paste. The route is optional and coexists with OpenAI API keys and all existing providers.

## Setup

```bash
npx openai-oauth@latest login
npx openai-oauth@latest --detach
medusa config
```

Choose **ChatGPT OAuth via local openai-oauth gateway**. Medusa connects only to `http://127.0.0.1:10531/v1`; the gateway owns login, refresh, credential storage, and account-aware model discovery. Medusa never reads `~/.codex/auth.json`.

## Other routes

Choose **OpenAI API key** to use `https://api.openai.com/v1` with `OPENAI_API_KEY`, or select another direct, local, or OpenAI-compatible provider. OAuth is an option, not a mandatory default.

## Security boundary

Keep the gateway bound to loopback. Do not expose port 10531 to a LAN or the internet. Existing Medusa tool approvals, sandboxing, and policy checks remain authoritative regardless of model route.
