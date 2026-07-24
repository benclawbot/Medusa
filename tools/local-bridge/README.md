# Medusa Local Execution Bridge

This bridge lets an orchestrator request a small, explicit set of Git, Cargo,
and GitHub CLI operations on the machine that owns the repository checkout.
It is intended to fill execution gaps in remote connectors without exposing an
arbitrary shell.

## Security model

- Listens on `127.0.0.1` by default.
- Requires a bearer token of at least 32 characters.
- Executes only named actions from a static allowlist.
- Never uses `shell=True` or evaluates command strings.
- Runs every action from one resolved repository root.
- Starts read-only unless `--allow-mutation` is supplied.
- Removes secrets and most inherited environment variables.
- Rejects Git configuration and executable override arguments.
- Serializes commands to avoid concurrent repository mutation.
- Limits request and command-output sizes.
- Records every accepted or rejected request in `.git/medusa-bridge-audit.jsonl`.

Do not bind this service to a public interface. The bearer token authorizes
repository mutations when mutation mode is enabled.

## Install

From a Medusa checkout:

```sh
./tools/local-bridge/install.sh "$PWD"
```

The installer copies the bridge to `~/.local/share/medusa/local-bridge`, creates
a private token in `~/.config/medusa/local-bridge-token`, and installs the
`medusa-local-bridge` launcher in `~/.local/bin`.

Start it:

```sh
~/.local/bin/medusa-local-bridge
```

Start in read-only mode without the installer-generated launcher:

```sh
python3 tools/local-bridge/medusa_bridge.py \
  --repo "$PWD" \
  --token-file ~/.config/medusa/local-bridge-token
```

## API

Health and action discovery:

```sh
TOKEN=$(cat ~/.config/medusa/local-bridge-token)
curl -sS \
  -H "Authorization: Bearer $TOKEN" \
  http://127.0.0.1:8765/health
```

Run an action:

```sh
curl -sS \
  -H "Authorization: Bearer $TOKEN" \
  -H 'Content-Type: application/json' \
  -d '{"action":"cargo.fmt","args":[]}' \
  http://127.0.0.1:8765/v1/run
```

Fix and validate PR #220 locally:

```sh
curl -sS -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
  -d '{"action":"git.checkout","args":["agent/time-travel-snapshots"]}' \
  http://127.0.0.1:8765/v1/run

curl -sS -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
  -d '{"action":"cargo.fmt","args":[]}' \
  http://127.0.0.1:8765/v1/run

curl -sS -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
  -d '{"action":"cargo.generate-lockfile","args":[]}' \
  http://127.0.0.1:8765/v1/run

curl -sS -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
  -d '{"action":"cargo.test","args":[]}' \
  http://127.0.0.1:8765/v1/run
```

Commit and push after reviewing the diff:

```sh
curl -sS -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
  -d '{"action":"git.add","args":["Cargo.lock","crates"]}' \
  http://127.0.0.1:8765/v1/run

curl -sS -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
  -d '{"action":"git.commit","args":["-m","Fix formatting and refresh dependency lock"]}' \
  http://127.0.0.1:8765/v1/run

curl -sS -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
  -d '{"action":"git.push","args":["origin","agent/time-travel-snapshots"]}' \
  http://127.0.0.1:8765/v1/run
```

## Tests

```sh
cd tools/local-bridge
python3 -m unittest -v test_bridge.py
python3 -m py_compile medusa_bridge.py test_bridge.py
```

The bridge uses only the Python standard library.
