# Local execution bridge

The bridge lets an orchestrator request a small, audited set of maintenance commands on the machine that owns the Medusa checkout. It does not provide an arbitrary shell.

## Start the bridge

From the Medusa repository:

```bash
python3 tools/local_bridge.py --repo "$PWD"
```

The process prints `MEDUSA_BRIDGE_URL` and a random `MEDUSA_BRIDGE_TOKEN`. Keep the bridge bound to loopback and do not publish the token.

To use a stable token for an automation session:

```bash
export MEDUSA_BRIDGE_TOKEN="$(python3 -c 'import secrets; print(secrets.token_urlsafe(32))')"
python3 tools/local_bridge.py --repo "$PWD"
```

## Run commands

In another terminal, export the URL and token printed by the server, then use the client:

```bash
python3 tools/medusa_bridge_client.py git_status
python3 tools/medusa_bridge_client.py cargo_fmt
python3 tools/medusa_bridge_client.py cargo_generate_lockfile
python3 tools/medusa_bridge_client.py cargo_clippy
python3 tools/medusa_bridge_client.py cargo_test
python3 tools/medusa_bridge_client.py gh_pr_checks 220
```

The currently allowlisted commands are returned by `GET /health`.

## Repair PR #220

On the `agent/time-travel-snapshots` checkout:

```bash
python3 tools/medusa_bridge_client.py cargo_fmt
python3 tools/medusa_bridge_client.py cargo_generate_lockfile
python3 tools/medusa_bridge_client.py cargo_fmt_check
python3 tools/medusa_bridge_client.py cargo_check
python3 tools/medusa_bridge_client.py cargo_clippy
python3 tools/medusa_bridge_client.py cargo_test
python3 tools/medusa_bridge_client.py git_status
```

Review the resulting diff before committing and pushing. The bridge intentionally does not expose `git commit`, branch deletion, reset, force-push, or an arbitrary command endpoint.

## Security properties

- Loopback binding only.
- Bearer-token authentication using constant-time comparison.
- One configured repository root resolved at startup.
- Fixed executable and argument allowlist.
- No shell interpolation.
- Request, output, argument-count, argument-size, and execution-time limits.
- Non-interactive Git behavior.
- Repository-relative path validation for diff requests.

The bridge inherits the permissions of the local user running it. Run it only for the duration of a maintenance session and stop it with `Ctrl+C` afterward.
