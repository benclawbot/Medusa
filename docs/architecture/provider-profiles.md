# Provider profiles and validated mutations

Named provider profiles extend the shared configuration authority from issue #574 without changing runtime ownership.

## Storage

The default profile remains the backward-compatible user file:

```text
<config-root>/medusa/provider.toml
```

Named profiles are stored separately:

```text
<config-root>/medusa/profiles/<name>.toml
```

The optional active selector is:

```text
<config-root>/medusa/active-profile.toml
```

When the selector is absent, `default` is active. Selecting a named profile changes only the small selector document; it does not copy or partially merge profile contents. The shared `medusa-config` loader resolves the active profile before applying it to runtime configuration.

Profile names are restricted to bounded ASCII letters and digits, with `-` and `_` allowed after the first character. Path separators, traversal syntax, Unicode lookalikes, and empty names are rejected.

## CLI surface

```text
medusa config set <key> <value>
medusa config unset <key>
medusa config profiles list [--json]
medusa config profiles create <name>
medusa config profiles use <name>
medusa config profiles delete <name>
```

Creating a profile copies the current active non-secret provider configuration. Deleting the built-in `default` profile or the currently active named profile is rejected.

`set` and `unset` operate only on the known secret-free provider-profile keys. They construct and validate the complete candidate profile before atomic persistence, so invalid enum values, malformed endpoints, empty required identifiers, and inconsistent OpenAI routes never replace the prior valid file.

Selecting `chatgpt-oauth` or `openai-api` applies the required provider, authentication, and endpoint tuple as one candidate change. Credentials remain outside profile documents.

Profile catalog tests cover isolation, active selection, deletion safety, and complete-candidate mutation validation. The authoritative repository matrix then exercises workspace policy, runtime integration, and packaged CLI behavior across supported platforms.

## Follow-up boundary

This slice does not add provider secrets, OAuth login/logout commands, generic runtime-policy mutation, TUI or desktop forms, Telegram callbacks, configuration revisions, or cross-frontend change events. Those surfaces must consume the same catalog and validation APIs rather than create independent profile state.
