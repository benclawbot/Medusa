# Provider image capability contract

Medusa treats image input as an explicit property of the effective provider route, endpoint, and model. A provider brand alone is not evidence that the selected route accepts images.

For every `MessageBlock::Image`, a provider adapter must do exactly one of the following before an HTTP request is sent:

1. encode the image into the provider request; or
2. return a validation error identifying the active provider, model, endpoint, and unsupported content type.

Provider serializers repeat this validation as a defense-in-depth boundary. A wildcard or no-op image match arm is not permitted.

## Current routes

- Anthropic uses its declared image capability and validates configured limits before submission.
- Anthropic-compatible routes default to text-only unless image support is explicitly enabled by their route configuration.
- OpenAI-compatible routes currently default to text-only. Image transport support is implemented separately because public API and ChatGPT OAuth/Codex transports do not share an identical wire contract.
- MiniMax image input can be enabled with `MINIMAX_IMAGE_INPUT=true` only when the configured endpoint and model are known to accept the Anthropic-compatible image payload.

Capability overrides must be conservative. When endpoint or model support is uncertain, leave image input disabled so Medusa fails visibly rather than sending a text-only request that silently omits the attachment.
