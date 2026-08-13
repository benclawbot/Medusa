# Provider delivery contract

Selectable route support and live-dogfood status are governed by `docs/provider-support.json`. This document describes the first-run diagnostic contract and must not promote a route beyond that manifest.

Medusa keeps first-run provider setup intentionally small and separates deterministic compatibility checks from optional live-provider tests.

## Supported first-run routes

The stable setup surface is:

- ChatGPT/OpenAI through API key or the local OAuth gateway;
- Anthropic through API key;
- a local OpenAI-compatible runtime;
- an explicit advanced/custom endpoint.

Other provider names are treated as advanced routes and must not be presented as supported defaults without corresponding adapter evidence.

## Preflight diagnostic

Run the diagnostic before starting a coding session:

```bash
cargo provider-diagnostic
```

To validate a specific configuration without reading the user profile:

```bash
cargo provider-diagnostic --config examples/provider-diagnostic/local.toml --output provider-diagnostic.json
```

The command emits a versioned JSON report covering:

- authentication configuration and credential presence;
- model and protocol compatibility;
- tool-use capability;
- image-input support;
- configured context window;
- streaming claims;
- external sidecars or local runtimes;
- whether a minimal completion route can be attempted.

The deterministic diagnostic never prints credential values and does not contact a provider. Live availability and completion checks remain optional credentialed canaries and must record provider, model, configuration, and run metadata separately.

## Fail-closed behavior

The diagnostic exits unsuccessfully when:

- the provider is outside the supported first-run set;
- an API-key route has no matching environment credential;
- a custom route omits `base_url`;
- the protocol or authentication mode is unsupported;
- the model is empty;
- configuration advertises streaming that the production adapter contract does not guarantee.

Image input is reported as unsupported by the stable setup contract. Tool use is reported only when it is enabled and the configured protocol supports the production tool-call path.

## External dependencies

ChatGPT OAuth uses the loopback `openai-oauth` gateway. Local routes require the configured local OpenAI-compatible runtime. These dependencies are reported explicitly rather than started silently by the diagnostic.

## Distribution trust boundary

The existing draft-release workflow provides immutable tag validation, clean platform builds, checksums, a deterministic CycloneDX SBOM, and GitHub/Sigstore provenance attestations. These controls do not replace platform signing:

- Windows binaries and installers still require Authenticode certificates;
- macOS applications still require Developer ID signing and notarization credentials;
- Linux repository distribution still requires signed repository metadata.

Until those credentials and custody procedures are configured, public documentation and diagnostics must describe release artifacts as unsigned platform packages with checksums and provenance—not as generally trusted signed releases.
