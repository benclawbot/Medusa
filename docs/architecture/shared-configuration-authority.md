# Shared configuration authority

Issue #574 moves Medusa toward one configuration authority used by every frontend.

The first production slice retains the existing `provider.toml` compatibility format but changes ownership:

```text
CLI / future TUI / desktop / Telegram
  -> medusa-config::ProviderProfileStore
  -> validated, atomic provider profile persistence
  -> medusa-config::Config layered runtime resolution
```

`medusa-cli` remains responsible for interactive prompts and starting the Codex app-server OAuth flow, but it no longer defines or persists a second provider-profile schema.

## Provider profile contract

The shared store owns:

- the platform-specific user configuration path;
- the secret-free provider profile schema;
- validation for connection, authentication, model, and endpoint combinations;
- atomic temporary-file replacement and directory synchronization;
- reset behavior;
- stable keyed inspection for non-secret fields.

The compatible keys are:

- `connection`
- `provider`
- `model`
- `speed`
- `reasoning`
- `auth`
- `base_url`
- `configured`

Unknown fields fail closed. Credentials, OAuth tokens, Telegram bot tokens, and API keys are not part of this document and cannot be returned by `show` or `get`.

## CLI surface in this slice

```text
medusa config
medusa config init
medusa config show [--json]
medusa config get <key> [--json]
medusa config validate [--json]
medusa config reset
```

Running `medusa config` remains equivalent to the interactive initialization flow.

The validation output identifies whether the profile is configured, the resolved provider/model protocol, and whether the existing OpenAI OAuth route is selected. Validation does not perform billable model work and does not reveal credential material.

The focused implementation gate formats the workspace and runs tests and Clippy for both `medusa-config` and `medusa-cli` before the source commit is published. Repository-wide CI remains the authoritative cross-platform acceptance gate.

## Follow-up surfaces

Profiles, generic `set`/`unset`, secret storage, TUI `/config`, desktop Settings, Telegram `/config`, doctor integration, configuration revisions, and cross-frontend change events remain follow-up slices. They must build on this shared store rather than reintroducing frontend-owned configuration state.
