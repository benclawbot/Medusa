# Model Selection Parity

Medusa's interactive model switcher now exposes the same provider routes accepted by first-run configuration: MiniMax, Anthropic, Anthropic-compatible, OpenAI, ChatGPT OAuth through the Codex app-server, OpenAI-compatible endpoints, OmniRoute, and local OpenAI-compatible runtimes.

Changing providers updates both the provider identifier and the wire protocol. Anthropic-family routes use the Anthropic Messages protocol; the remaining routes use the OpenAI-compatible chat protocol.

API keys entered during first-run setup or model configuration are saved in the operating system's secure credential store and forwarded to the daemon only for the active Medusa process. Provider environment variables remain supported. ChatGPT OAuth, OmniRoute, and local gateways do not require Medusa to read an OAuth credential file.
