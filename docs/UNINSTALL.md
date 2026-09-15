# Safe uninstall

`medusa uninstall` cleans only state named by Medusa's closed uninstall ownership manifest. It
never removes source files, arbitrary files below `.medusa`, or user configuration outside the
selected repository.

The default command is conservative:

```console
medusa --repo PATH uninstall
```

It removes known ephemeral runtime caches and preserves durable sessions, configuration, skills,
and every state path whose ownership is unknown. Use `--preview` (or `--dry-run`) to inspect the
same bounded report without changing files. Add `--json` for automation.

Durable data requires an exact scope and an explicit purge request:

```console
medusa --repo PATH uninstall --preview --scope sessions
medusa --repo PATH uninstall --purge-data --scope sessions
medusa --repo PATH uninstall --purge --scope configuration
medusa --repo PATH uninstall --purge --scope skills
```

The supported scopes are `runtime`, `cache`, `sessions`, `configuration`, `skills`, and `all`.
`--purge-data` is an alias for `--purge`; it does not broaden the scope. `--purge` without
`--scope` is rejected, and `--scope` without `--preview` or `--purge` is also rejected.

Uninstall fails closed for a symlinked state root or target, a linked Git worktree, unknown
filesystem entries, and traversal outside the bounded report/scan limits. A purge continues
through independent entries when one removal fails, then emits the paths removed, preserved,
protected, and failed. A non-success result means the report must be reviewed before retrying.

The command manages repository-local Medusa state only. Removing the installed executable remains
the responsibility of the package manager or installer that installed it. Running uninstall does
not remove the global provider profile, credentials, or user-owned configuration.
