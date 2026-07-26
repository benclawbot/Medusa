# Model Selection Parity

Medusa's interactive model switcher now exposes the same provider routes accepted by first-run configuration: MiniMax, Anthropic, Anthropic-compatible, OpenAI, ChatGPT OAuth through the configured local gateway, OpenAI-compatible endpoints, OmniRoute, and local OpenAI-compatible runtimes.

Changing providers updates both the provider identifier and the wire protocol. Anthropic-family routes use the Anthropic Messages protocol; the remaining routes use the OpenAI-compatible chat protocol.

Credentials remain process-local or environment-backed. ChatGPT OAuth, OmniRoute, and local gateways do not require Medusa to read an OAuth credential file.