# Interchangeable GitHub Management

Medusa exposes one versioned, repository-confined GitHub operation contract through `medusa-github` and the packaged `medusa-github-operation` entrypoint. Callers choose a typed operation. They do not assemble REST paths, shell commands, or backend-specific arguments.

## Backends

The same schema-version 2 document can select:

- `native_cli` — prefers a safe dedicated `gh` command with structured JSON output and falls back to repository-scoped `gh api` when no native mapping exists;
- `gh_api` — executes through repository-scoped `gh api` using the GitHub CLI credential store;
- `direct_oauth` — executes directly over bounded HTTPS with a GitHub App user access token stored in the operating-system credential store.

A backend changes transport only. Authorization, risk classification, request identity, idempotency, reconciliation, repository confinement, redaction, response limits, audit evidence, and canonical receipt fields do not change.

## Typed operation document

```json
{
  "schemaVersion": 2,
  "repository": "acme/project",
  "hostname": "github.com",
  "apiVersion": "2022-11-28",
  "backend": "direct_oauth",
  "idempotencyKey": "issue-import-2026-08-02-001",
  "maxResponseBytes": 1048576,
  "operation": {
    "resource": "issues",
    "operation": {
      "action": "create",
      "title": "Unexpected shutdown",
      "body": "Reproduction details",
      "labels": ["bug"],
      "assignees": []
    }
  }
}
```

Run it with:

```bash
medusa github execute --request operation.json --approve
```

Use `--request -` to read the document from standard input. Direct OAuth operations require a public GitHub App client ID through `--client-id` or `MEDUSA_GITHUB_CLIENT_ID`.

The old REST-shaped document remains accepted only through an exact, fail-closed compatibility adapter. Known legacy operations are translated into a typed operation and emit a deprecation warning. Arbitrary legacy endpoints are rejected.

## Resources

The typed contract covers:

- repository creation, metadata, settings, topics, and deletion;
- contents, branches, refs, and commits;
- issues, comments, labels, assignees, reactions, and locks;
- pull requests, reviews, reviewers, closure, and merge strategies;
- Actions runs, jobs, reruns, cancellation, and artifact download;
- releases, release assets, and tags;
- collaborators and repository permissions;
- environments, variables, and encrypted secrets;
- webhooks;
- branch protection and typed repository rulesets;
- repository projects;
- repository-scoped issue, commit, and code search.

Backends construct the endpoint or command internally. A typed caller cannot supply an arbitrary URL or REST path. Search scope is added internally and callers cannot add another `repo:`, `org:`, or `user:` qualifier.

## Authorization

Medusa classifies each typed operation before execution:

| Risk | Examples | Required flags |
|---|---|---|
| `read_only` | repository metadata, issue listing, workflow inspection | none |
| `mutation` | create/update issues, comments, file writes, release upload | `--approve` |
| `administration` | collaborators, branch protection, hooks, environments, repository settings | `--approve --approve-high-risk` |
| `secret` | Actions or environment secret operations | `--approve --approve-high-risk` |
| `destructive` | repository, resource, asset, or configuration deletion | `--approve --approve-high-risk` |

The standard capability authorizer records each decision before dispatch. A high-risk approval cannot substitute for the ordinary mutation approval, and vice versa.

## Identity and idempotency

Every execution has:

- a unique ULID-based attempt ID;
- a SHA-256 request digest over the complete semantic typed request, including mutation bodies and artifact metadata;
- an optional hashed caller idempotency key.

The digest excludes the selected backend so equivalent operations can be compared across transports.

The durable ledger records `requested`, `authorized`, `dispatched`, `accepted`, `completed`, `persisted`, `uncertain`, `reconciled`, and `failed` states.

- An exact repeated key and digest returns the previous successful receipt without another GitHub mutation.
- Reusing a key with another digest fails closed.
- A dispatched mutation whose result became uncertain is reconciled before any retry.
- Non-idempotent mutations are not repeated when reconciliation is inconclusive.

Typed reconciliation uses hidden issue/PR request markers, branch refs and SHAs, content presence or absence, release tags, and resource absence checks.

## Capability negotiation

`medusa github capabilities --request operation.json` evaluates the exact request before dispatch and reports:

- backend availability and authentication state;
- authenticated user where known;
- host and pinned API version;
- repository role and observable permissions;
- supported operation groups;
- missing permission remediation;
- rate-limit metadata;
- artifact transfer support;
- credential backend.

The direct OAuth backend rejects a mutation before dispatch when the repository permission is insufficient.

## Receipts

The schema-version 2 receipt contains:

- attempt ID, request digest, and hashed idempotency key;
- lifecycle and reconciliation state;
- repository, host, typed operation, risk, selected backend, and actual transport;
- resource identity and URL;
- mutation status;
- HTTP status and `X-GitHub-Request-Id` where available;
- retry count and reason;
- rate-limit and `Retry-After` metadata;
- pinned/selected API version, deprecation, sunset, and ETag metadata;
- bounded backend-independent canonical fields;
- bounded redacted response payload;
- optional artifact metadata containing path, byte count, SHA-256, media type, and filename;
- truncation, redaction, replay, and audit-persistence flags.

Binary contents never appear in JSON receipts.

## Retry and recovery

The direct backend performs bounded exponential backoff with deterministic jitter only for reads and typed operations explicitly marked idempotent. It captures GitHub's request and rate-limit headers on every response.

The idempotency ledger is stored under `$MEDUSA_HOME/state/github-operations-ledger.jsonl`, or `.medusa/state/github-operations-ledger.jsonl` when `MEDUSA_HOME` is unset. The raw idempotency key is not stored. Authorization events are stored separately under the audit directory.

## Artifact transfer

Actions artifacts and release assets use first-class typed operations.

Downloads:

- require a repository-relative destination;
- stream into a temporary file in the target directory;
- enforce byte limits while reading;
- calculate SHA-256 while writing;
- atomically persist the result;
- follow only a bounded number of HTTPS redirects;
- do not forward the bearer token to redirected object storage.

Release uploads:

- require ordinary mutation approval;
- require a source inside the repository root;
- reject symlink escapes;
- enforce a byte limit before dispatch;
- stream directly from the file with an explicit content length;
- never put asset bytes in a JSON body or command argument.

## OAuth

`direct_oauth` uses a GitHub App device flow and rotating user access token. See [GITHUB-OAUTH.md](GITHUB-OAUTH.md) for setup, token refresh, keyring storage, Enterprise URLs, and troubleshooting.

No client secret is accepted. No raw token input is accepted. There is no plaintext credential fallback.

## Security boundaries

- No arbitrary endpoint, host, or shell command.
- No raw token or client-secret input.
- No token in command arguments, logs, receipts, or durable events.
- No cross-repository operation hidden in search or path fields.
- Mutation bodies use temporary JSON input for `gh api`.
- Direct responses are bounded before memory buffering.
- Credentials and secret values are redacted before receipts or audit persistence.
- Request bodies are bounded.
- Response sizes are caller-bounded to a hard maximum.
- Artifact destinations and upload sources remain repository-confined.
- The configured coordinator repository and hostname must exactly match the typed document.

Repository creation remains available through its specialized bootstrap workflow and is also represented in the typed operation schema. Its local initialization, idempotent remote reuse, and partial-failure recovery behavior remain intact.
