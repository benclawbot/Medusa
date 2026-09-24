# TUI configuration command

The TUI `/config` command is a frontend over the shared configuration authority from issue #574. It does not introduce a TUI-owned settings file, secret cache, or parallel model configuration path.

## Command surface

```text
/config
/config show
/config profiles
/config use <profile>
/config set <key> <value>
/config unset <key>
/config validate
```

The command is discoverable through the existing slash-command help and completion system. Output is delivered through normal runtime notices, while successful model-route changes also emit the existing typed settings event.

## Mutation boundary

Profile selection and key mutations follow this order:

1. load the target or active shared provider profile;
2. construct the complete candidate profile;
3. validate the profile itself;
4. resolve and validate the effective runtime configuration with project configuration still taking precedence;
5. persist the profile or active selector atomically;
6. refresh the live runtime model route;
7. emit redacted confirmation and typed settings state.

A failed candidate never replaces the previous profile or live route.

Configuration changes preserve process-only state such as the current effort setting, planning mode, and session state. API keys are never displayed or written to `provider.toml`; keys entered in setup or the model picker are stored in the operating system's secure credential store and sent to the daemon over its local credential channel when needed.

## Redaction and provider behavior

`/config` shows the active profile, profile path, stored route, effective route, protocol, authentication mode, base URL, and configured status. It never shows API keys, OAuth credentials, or secret-store values.

`/config validate` performs no provider request and therefore cannot incur model usage. It validates the same layered configuration that the runtime would use for the next turn.

Focused regression coverage proves that invalid effective mutations do not replace the prior profile, profile selection refreshes the live model while preserving process-only state, project model overrides retain higher precedence, and status output excludes process-scoped credentials. Repository-wide CI remains the authoritative cross-platform acceptance gate.

## Follow-up boundary

This slice does not add secret-store login/logout, configuration revision conflicts, or cross-frontend change subscriptions. Those surfaces must consume the same shared catalog and effective-config validation APIs.
