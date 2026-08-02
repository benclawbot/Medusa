# GitHub App OAuth for Medusa

Medusa supports a direct HTTPS GitHub backend authenticated with a GitHub App user access token. This backend does not require the `gh` executable and does not accept a personal access token, OAuth token, refresh token, client secret, or password through configuration, standard input, command-line arguments, operation documents, logs, receipts, or audit records.

## Why a GitHub App

GitHub Apps provide repository-scoped, fine-grained permissions and can issue short-lived user access tokens. Medusa uses the OAuth 2.0 device flow because it is suitable for a CLI or headless local agent and does not require Medusa to receive an inbound browser callback.

Create or select a GitHub App that:

1. is owned by the user or organization that controls the repositories Medusa will manage;
2. has **Device Flow** enabled;
3. requests only the repository permissions needed for the typed operations Medusa will perform;
4. is installed on the exact repositories or organization scope that Medusa may access;
5. uses expiring user access tokens when that option is available.

The GitHub App **client ID is public configuration**. Do not provide Medusa with the GitHub App client secret. Device flow and refresh-token rotation do not require a client secret in Medusa's public-client design.

GitHub documentation:

- [Authorizing OAuth apps with device flow](https://docs.github.com/apps/oauth-apps/building-oauth-apps/authorizing-oauth-apps#device-flow)
- [Generating a user access token for a GitHub App](https://docs.github.com/apps/creating-github-apps/authenticating-with-a-github-app/generating-a-user-access-token-for-a-github-app)
- [Refreshing user access tokens](https://docs.github.com/apps/creating-github-apps/authenticating-with-a-github-app/refreshing-user-access-tokens)
- [About authentication with a GitHub App](https://docs.github.com/apps/creating-github-apps/authenticating-with-a-github-app/about-authentication-with-a-github-app)

## Login

```bash
medusa github auth login \
  --client-id Iv1.examplePublicClientId \
  --hostname github.com
```

The command prints GitHub's verification URL and a short user code, then polls the token endpoint at GitHub's required interval. `authorization_pending`, `slow_down`, denial, expiration, and disabled-device-flow responses are handled explicitly.

The user code is not a reusable credential. The access and refresh tokens are never printed.

The public client ID may instead be supplied through:

```bash
export MEDUSA_GITHUB_CLIENT_ID=Iv1.examplePublicClientId
```

There is intentionally no `MEDUSA_GITHUB_CLIENT_SECRET` setting.

## Credential storage

After authorization, Medusa stores one serialized credential in the operating-system credential store:

- macOS: Keychain;
- Windows: Credential Manager;
- Linux: Secret Service through the persistent native keyring backend.

Repository files, `.medusa`, ordinary configuration files, shell history, and operation documents never contain the token. If the operating-system credential store cannot initialize or persist the credential, login fails closed. There is no plaintext fallback.

The keyring entry is namespaced by GitHub hostname and public client ID so separate GitHub Apps and Enterprise installations do not overwrite one another.

## Status, refresh, and logout

```bash
medusa github auth status --client-id Iv1.examplePublicClientId
medusa github auth refresh --client-id Iv1.examplePublicClientId
medusa github auth logout --client-id Iv1.examplePublicClientId
```

Status reports only:

- whether a credential exists and is currently usable;
- the authenticated login when GitHub returned it;
- access-token and refresh-token expiration timestamps;
- whether refresh is available or already required;
- the credential-store backend;
- the pinned REST API version.

Refresh uses the current refresh token and atomically replaces both the access token and refresh token with GitHub's rotated values. Access-token retrieval automatically refreshes when the credential is close to expiration. An expired or missing refresh token requires a new login.

Logout deletes the keyring entry. It does not print or return the deleted credential.

## Direct typed operation

Use `direct_oauth` in a schema-version 2 operation document:

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

Execute it with:

```bash
medusa github execute \
  --request operation.json \
  --client-id Iv1.examplePublicClientId \
  --approve
```

Administrative, secret, and destructive operations require both approvals:

```bash
medusa github execute \
  --request branch-protection.json \
  --client-id Iv1.examplePublicClientId \
  --approve \
  --approve-high-risk
```

The CLI rejects `--approve-high-risk` as a substitute for the ordinary mutation approval.

## Capability and permission preflight

```bash
medusa github capabilities \
  --request operation.json \
  --client-id Iv1.examplePublicClientId
```

The report includes:

- backend availability and authentication state;
- authenticated login where known;
- hostname and pinned REST API version;
- repository role and observable repository permissions;
- exact operation groups supported by the backend;
- missing permissions that would prevent dispatch;
- current rate-limit information returned by GitHub;
- artifact-download and release-upload support;
- credential-store backend.

Medusa evaluates this report before every direct mutation. A missing repository permission fails before the mutation request is sent.

## GitHub Enterprise Server and GHE.com

Set the hostname and, where the installation does not use the standard paths, the OAuth and API base URLs:

```bash
medusa github auth login \
  --client-id Iv1.enterpriseClientId \
  --hostname github.example.com \
  --oauth-base-url https://github.example.com \
  --api-base-url https://github.example.com/api/v3
```

The equivalent fields may be included in a typed operation document as `oauthBaseUrl` and `apiBaseUrl`. They must be absolute HTTPS URLs without embedded credentials, query strings, or fragments.

Medusa never follows an OAuth or API request to an insecure URL. Binary artifact downloads may follow a bounded number of HTTPS redirects. The bearer token is not forwarded to the redirected object-storage host.

## API behavior and evidence

The direct backend:

- sends `Authorization: Bearer …` only in the HTTP header;
- pins `X-GitHub-Api-Version`;
- requests GitHub's JSON media type;
- bounds response bytes while reading the network stream;
- records HTTP status, `X-GitHub-Request-Id`, rate-limit headers, `Retry-After`, selected API version, deprecation, sunset, and ETag metadata;
- retries only reads and typed operations explicitly classified as idempotent;
- uses bounded exponential backoff with deterministic jitter;
- redacts token-like strings and secret fields before creating a receipt or durable event.

The receipt contains a unique attempt ID and a transport-neutral SHA-256 digest of the full semantic request. The digest includes mutation bodies and artifact metadata but excludes the selected backend, allowing backend-conformance comparisons.

## Idempotency and uncertain outcomes

An optional `idempotencyKey` is hashed before durable storage. The raw key is never written to the ledger.

- Repeating the same key with the same request digest returns the prior successful receipt without another GitHub mutation.
- Reusing a key with a different request digest fails closed.
- Each dispatch records lifecycle states such as `requested`, `authorized`, `dispatched`, `accepted`, `completed`, and `persisted`.
- A connection loss after dispatch records `uncertain`.
- Typed create/delete operations use operation-specific reconciliation, such as hidden issue/PR markers, branch ref and SHA checks, content presence/absence, or release tags.
- Medusa does not blindly repeat a non-idempotent mutation when reconciliation is inconclusive.

The bounded ledger is stored under `$MEDUSA_HOME/state/github-operations-ledger.jsonl`, or under the repository's `.medusa/state` directory when `MEDUSA_HOME` is unset. Authorization decisions are stored separately under the audit directory.

## Artifact transfer

Actions artifacts and release assets use typed transfer operations. Downloads:

- require repository-relative destinations;
- use a temporary file in the destination directory;
- enforce a configured byte limit while streaming;
- calculate SHA-256 while writing;
- sync and atomically persist the final file;
- return metadata rather than binary data in the JSON receipt.

Release uploads:

- require an approved mutation;
- require a source file inside the repository root;
- reject symlink escapes;
- enforce a configured byte limit before dispatch;
- stream the file from disk with an explicit content length;
- never copy the complete asset into a JSON body, receipt, or process argument.

## Security invariants

- No raw token input.
- No client-secret input.
- No plaintext credential fallback.
- No token in command arguments.
- No arbitrary endpoint or host supplied by a typed operation.
- No cross-repository search qualifier supplied by the caller.
- No shell execution.
- No mutation without the applicable approvals.
- No direct mutation when capability preflight proves the repository permission is insufficient.
- No automatic retry of a non-idempotent mutation after an uncertain dispatch.
- No binary artifact content in JSON receipts or audit records.
