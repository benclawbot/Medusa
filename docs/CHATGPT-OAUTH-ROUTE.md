# ChatGPT OAuth route

Medusa can use ChatGPT/Codex through the local `openai-oauth` gateway without API access or manual copy/paste. The route is optional and coexists with OpenAI API keys and all existing providers.

## Setup

```bash
medusa config
```

Choose **ChatGPT OAuth via local openai-oauth gateway**. Medusa checks `http://127.0.0.1:10531/v1` whenever an interactive, run, or resume session starts. If the gateway is not running, Medusa starts it through:

```bash
npx --yes openai-oauth@latest --detach
```

The gateway owns browser login, token refresh, credential storage, and account-aware model access. Medusa never reads `~/.codex/auth.json`.

A user can also log in or manage the gateway directly:

```bash
npx openai-oauth@latest login
npx openai-oauth@latest status
npx openai-oauth@latest stop
```

## OpenAI API option

Choose **OpenAI API key** to use `https://api.openai.com/v1` with `OPENAI_API_KEY`. OAuth is an option, not a mandatory default. MiniMax, Anthropic, OmniRoute, local runtimes, and custom OpenAI-compatible endpoints remain supported.

## Removed manual route

The `medusa escalate` browser/copy-paste command and its documentation are removed from the product surface. Existing internal escalation packet types remain only for workspace compatibility and are not used by the OAuth route.

## Security boundary

Keep the gateway bound to loopback. Do not expose port `10531` to a LAN or the internet. Existing Medusa tool approvals, sandboxing, and policy checks remain authoritative regardless of the selected model route.
