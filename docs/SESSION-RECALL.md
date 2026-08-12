# Session recall

Medusa keeps a repository-local recall index at `.medusa/session-recall.sqlite3`. The index is populated from durable session recall records and can be inspected through either the primary CLI or the companion binary.

## List recent sessions

Use the chronological history view to inspect the newest recorded sessions:

```text
medusa recall list
medusa-recall list
```

The default limit is 20 sessions. Results are ordered by `created_at` descending, then by session ID for deterministic ties.

```text
medusa recall list --limit 50
```

Each human-readable entry includes:

- session ID
- creation timestamp
- outcome
- repository fingerprint
- event count
- tools used
- parent session ID when present

An empty or not-yet-created recall database is treated as an empty history rather than an error.

## Filters

Filters can be combined:

```text
medusa recall list --outcome success
medusa recall list --tool shell
medusa recall list --repository sha256:repository-fingerprint
medusa recall list --from 2026-07-01T00:00:00Z --to 2026-07-31T23:59:59Z
medusa recall list --tool git --outcome success --limit 10
```

Supported filters:

| Option | Meaning |
|---|---|
| `--repository FINGERPRINT` | Match one repository fingerprint |
| `--tool NAME` | Require that a session used the named tool |
| `--outcome VALUE` | Match the recorded session outcome |
| `--from RFC3339` | Include sessions at or after the timestamp |
| `--to RFC3339` | Include sessions at or before the timestamp |
| `--limit N` | Return between 1 and 100 entries |

`--repo PATH` remains the global option selecting the repository whose `.medusa` state should be opened. It is separate from `--repository`, which filters stored metadata.

## JSON output

Use `--json` for scripts and tooling:

```text
medusa recall --json list --limit 5
```

Each entry contains:

```json
{
  "session_id": "ses-example",
  "parent_session_id": null,
  "created_at": "2026-07-20T20:00:00Z",
  "repository_fingerprint": "sha256:example",
  "outcome": "success",
  "tools": ["git", "shell"],
  "event_count": 12
}
```

## Search, open, and compare

Chronological listing complements the existing recall commands:

```text
medusa recall search "Windows Cargo replacement"
medusa recall open ses-example
medusa recall compare ses-before ses-after
```

Use `list` when no search terms are known, `search` for full-text retrieval, `open` for event detail, and `compare` for two-session outcome and tool differences.
