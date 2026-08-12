# Interchangeable GitHub Management

Medusa exposes one repository-confined GitHub operation contract through `medusa-github` and the `medusa-github-operation` production entrypoint. Callers choose a typed operation. They do not assemble shell commands or depend on which backend executes it.

## Backends

`native_cli` prefers a dedicated GitHub CLI command when Medusa has a normalized native mapping. Operations without such a mapping automatically use the repository-scoped `gh api` transport while preserving the selected backend identity in the receipt.

`rest_api` executes the same operation directly through `gh api`. Both backends use the GitHub CLI credential store, support GitHub.com and Enterprise hostnames, and return the same normalized receipt shape.

A backend changes transport only. It does not change authorization, endpoint confinement, redaction, response limits, audit evidence, or caller-visible canonical fields.

## Operation document

```json
{
  "repository": "acme/project",
  "hostname": "github.com",
  "resource": "issues",
  "action": "create",
  "method": "POST",
  "endpoint": "issues",
  "query": {},
  "body": {
    "title": "Unexpected shutdown",
    "body": "Reproduction details"
  },
  "backend": "native_cli",
  "paginate": false,
  "maxResponseBytes": 1048576
}
```

Run it with:

```bash
medusa-github-operation --request operation.json --approve
```

Use `--request -` to read the JSON document from standard input. Mutating request bodies are written to a temporary input file and passed to `gh api --input`; their contents are never placed in a shell command or command-line argument.

## Resources

The contract covers repository metadata and settings, contents, branches and Git data, issues and comments, pull requests and reviews, Actions runs/jobs/artifacts, releases and tags, collaborators, environments, variables, secrets, webhooks, branch protection, projects, and repository-scoped issue, commit, and code search.

Every endpoint is validated against its declared resource. Repository endpoints are built internally as `/repos/{owner}/{repository}/...`; callers cannot provide absolute URLs, schemes, alternate hosts, query strings inside endpoint paths, parent segments, or cross-repository paths. Search is the only global GitHub endpoint and requires the exact configured `repo:owner/name` term.

## Authorization

Medusa classifies each request before execution:

| Risk | Examples | Required flags |
|---|---|---|
| `read_only` | repository metadata, issue listing, workflow inspection | none |
| `mutation` | create/update issues, PR comments, file writes | `--approve` |
| `administration` | collaborators, branch protection, hooks, environments, repository settings | `--approve --approve-high-risk` |
| `secret` | Actions or environment secret operations | `--approve --approve-high-risk` |
| `destructive` | any `DELETE` operation | `--approve --approve-high-risk` |

The standard capability authorizer records each authorization decision. A high-risk approval cannot substitute for the ordinary mutation approval, and vice versa.

## Receipts and recovery

Each operation returns a stable JSON receipt containing:

- operation ID;
- repository and hostname;
- resource, action, method, and endpoint;
- risk classification;
- selected backend and actual transport;
- mutation status;
- normalized resource identity and URL when available;
- a backend-independent canonical payload;
- the bounded, redacted response payload;
- whether the response was truncated.

Receipts and authorization events are appended to `$MEDUSA_HOME/audit/github-operations.jsonl`, or `.medusa/audit/github-operations.jsonl` when `MEDUSA_HOME` is not configured. The audit destination is preflighted before a request is sent. If persistence fails after GitHub completed the operation, the receipt is still printed and the command returns a structured persistence error.

## Security boundaries

- No operation accepts a raw GitHub token.
- Credentials remain owned by the GitHub CLI secure credential store.
- No request is executed through a shell.
- Token-like values are rejected in query parameters.
- Credential fields and secret values are redacted from previews, errors, receipts, and audit records.
- Request bodies are limited to 1 MiB.
- Responses are bounded to a caller-selected limit no larger than 4 MiB.
- The configured service repository and hostname must exactly match the operation document.

Repository creation remains available through `medusa-capabilities create-repository`. It uses the same authenticated GitHub service boundary and retains its specialized bootstrap, idempotency, and partial-failure recovery contract.
