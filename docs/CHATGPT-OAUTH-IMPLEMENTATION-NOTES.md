# ChatGPT OAuth implementation notes

Medusa supports two distinct OpenAI routes:

- `chatgpt-oauth`: a local `openai-oauth` gateway at `127.0.0.1:10531`. Medusa checks the loopback endpoint at startup and starts the gateway with `npx openai-oauth@latest --detach` when needed.
- `openai-api`: the official OpenAI API endpoint using `OPENAI_API_KEY`.

Both routes use Medusa's existing OpenAI-compatible provider implementation. MiniMax, Anthropic, OmniRoute, local runtimes, and other compatible endpoints remain available.

The former user-facing browser/copy-paste escalation command has been removed. The old internal escalation packet library remains in the workspace for compatibility, but it is no longer exposed as a CLI route. ChatGPT access is now a normal selectable provider route with the same tool approvals, sandboxing, and policy enforcement as every other model provider.
