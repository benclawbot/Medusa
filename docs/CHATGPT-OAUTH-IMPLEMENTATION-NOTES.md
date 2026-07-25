# Implementation notes

This branch introduces first-class provider configuration for two distinct OpenAI routes:

- `chatgpt-oauth`: the local `openai-oauth` gateway at `127.0.0.1:10531`, started automatically through `npx` when selected.
- `openai-api`: the official OpenAI API endpoint using environment-provided API credentials.

The existing provider abstraction remains unchanged, so MiniMax, Anthropic, OmniRoute, local runtimes, and other OpenAI-compatible endpoints continue to work.

Follow-up work in this draft removes the legacy copy/paste escalation command and its browser/manual transport modules after CI confirms no remaining callers.
