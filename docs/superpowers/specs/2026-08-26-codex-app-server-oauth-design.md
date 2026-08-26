# Direct Codex App-Server OAuth

## Problem

Medusa currently treats ChatGPT OAuth as an OpenAI-compatible HTTP provider behind a
third-party loopback gateway. That makes the OAuth login and request path dependent on a
separate gateway process, leaves the TUI unable to recover cleanly when that process or its
client configuration is stale, and prevents Medusa from using the OAuth/session lifecycle that
the installed Codex CLI already owns.

The direct integration must use Codex's supported app-server protocol instead of reading or
extracting OAuth tokens. Codex app-server communicates over newline-delimited JSON-RPC on stdio;
it exposes account login, thread, turn, streaming, cancellation, and server-initiated approval
requests. It is an agent/session protocol, not an OpenAI Chat Completions endpoint.

## Goals

1. Route `openai-oauth` text/code turns through a direct `codex app-server --stdio` child.
2. Let Codex app-server own ChatGPT OAuth credentials, refresh, and browser callback handling.
3. Preserve Medusa's existing durable `AgentSession`, runtime events, frontend protocol, and
   question/approval UX.
4. Stream Codex assistant deltas into Medusa and persist the completed assistant message and
   model response metadata in the Medusa journal.
5. Persist the Codex thread identity with the Medusa session so a resumed Medusa session calls
   `thread/resume` instead of silently starting a second conversation.
6. Translate Codex command/file/permission approval requests into Medusa's existing blocking
   question path. A subsequent Medusa answer sends the corresponding JSON-RPC response back to
   Codex; no approval is silently auto-granted.
7. Use `turn/interrupt` for Medusa cancellation and fail closed if the app-server process or
   protocol becomes unavailable.
8. Replace OAuth gateway preflight/login/model discovery in CLI, TUI, and desktop paths with
   app-server operations.
9. Leave all non-`openai-oauth` provider and Medusa AgentEngine paths unchanged.

## Non-goals

- Do not read, parse, copy, or expose Codex's OAuth credential file or refresh token.
- Do not add a compatibility `/v1/chat/completions` proxy or retain the `openai-oauth` npm
  gateway as a fallback.
- Do not replace Medusa's provider engine for API-key, compatible-endpoint, local, or OmniRoute
  profiles.
- Do not reimplement Codex's built-in tools inside Medusa. Codex app-server remains the owner of
  its tool execution and sandbox; Medusa only presents lifecycle and approval events.
- Do not change the separate Realtime voice integration in this change. App-server-backed OAuth
  applies to normal text/code turns; Realtime capability remains a distinct route.

## Architecture

### Runtime client

`medusa-runtime` gains a small synchronous app-server client. It launches the locally installed
Codex executable (`codex.cmd` on Windows, `codex` elsewhere) with piped stdio and reads one JSON
object per line on a dedicated reader thread. Requests use monotonically increasing ids and are
bounded by receive timeouts. The client handles JSON-RPC responses, notifications, and
server-initiated requests without shell interpolation or token access. Dropping the client kills
and waits for its child.

The client performs the documented handshake (`initialize`, then `initialized`), checks
`account/read`, and starts `account/login/start` with `type: "chatgpt"` when needed. The returned
browser URL is opened directly by the platform launcher. Login completion is accepted only after
the matching `account/login/completed` notification reports success. `model/list` supplies the
authenticated model catalog for model pickers and preflight.

### OAuth turn path

When the effective primary provider is `openai-oauth`, `run_prompt` uses an OAuth-specific path;
all other providers continue through the existing `ConfiguredProvider` and `AgentEngine` loop.
The OAuth path creates or updates the normal Medusa `AgentSession`, then starts or resumes a
Codex thread with the configured model and repository working directory. The current Medusa
execution mode maps to a conservative Codex sandbox/approval policy: read-only sessions use
read-only sandboxing and no approval prompt, while review and yolo sessions use workspace-write
execution with on-request approvals. Codex remains the authority for the actual sandbox and
command execution.

`turn/start` receives text, local files, and validated image attachments in the app-server input
format. `item/agentMessage/delta` notifications become `RuntimeEvent::AssistantText` updates;
the final text is also appended as a normal assistant `Message` and recorded as
`AssistantMessageRecorded`. `turn/completed` statuses become Medusa completion, cancellation, or
failure events, and available usage is recorded through the existing response event shape.

Approval requests remain resumable: the client stores the pending JSON-RPC request id and method
in runtime memory, Medusa stores a corresponding `AgentQuestion` in the session, and the next
user submission resolves the request with `accept`, `acceptForSession`, or `decline`. If the
runtime process is restarted while an approval is pending, it fails closed with an actionable
message rather than guessing or granting access.

The Codex thread id is an optional, serde-defaulted field on `AgentSession`, preserving existing
session files and allowing older sessions to load. It is updated only after a successful
`thread/start`/`thread/resume` response and is persisted through the normal session snapshot.

### Frontends and startup

The CLI OAuth preflight calls the runtime app-server readiness/model discovery functions and no
longer probes or starts port `10531`. First-run setup and desktop provider-auth commands use the
same app-server login flow. OAuth profile copy and diagnostics are updated to describe the Codex
executable/auth store rather than a gateway. The existing `auth=none` display remains correct
because the credential is owned by Codex.

## Error handling and security

- Missing Codex executable: return a clear environment error naming `codex`/`codex.cmd` and keep
  the Medusa session durable.
- App-server startup, handshake, account, model, or turn protocol errors: surface a redacted
  Medusa failure; never print OAuth URLs, authorization codes, or raw credential material.
- Child stdout is treated as untrusted protocol input. Invalid JSON, oversized lines, unknown
  response ids, and malformed required fields fail closed or are ignored only when safe.
- Every wait for a response, login completion, or turn event has a finite timeout. Cancellation
  sends `turn/interrupt` and waits for the terminal `turn/completed` notification.
- Approval requests are never auto-approved because the client cannot prove that a requested
  command or permission is within Medusa's current user-visible scope.

## Verification

Unit tests cover JSONL framing, request/response correlation, account/login completion handling,
model-list parsing, turn status mapping, attachment conversion, approval decision mapping, and
Codex executable selection. Runtime tests use a scripted local app-server transport or fake
child protocol and assert durable session/thread binding, streaming text, cancellation, and
fail-closed approval recovery. Existing focused provider, runtime, daemon, TUI, desktop, and
configuration tests must remain green. A live OAuth test is optional and must never be included in
the default test suite.

## References

- OpenAI Codex app-server protocol: <https://github.com/openai/codex/blob/main/codex-rs/app-server/README.md>
- OpenAI Codex CLI: <https://github.com/openai/codex>
