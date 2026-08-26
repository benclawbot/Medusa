# Direct Codex App-Server OAuth Implementation Plan

> **Execution note:** Follow the incremental implementation and test-driven-development skills. Keep each slice compiling and run the focused tests immediately after it.

**Goal:** Replace Medusa's `openai-oauth` loopback gateway with the installed Codex CLI app-server while preserving Medusa sessions, approvals, frontends, and non-OAuth providers.

## 1. Establish the protocol boundary and durable state

**Files:**

- `crates/medusa-runtime/src/openai_oauth.rs`
- `crates/medusa-runtime/src/lib_wrapper.rs`
- `crates/medusa-agent/src/session.rs`
- all Rust `AgentSession { ... }` fixtures and constructors reported by `rg`

Add a synchronous JSONL app-server client that launches `codex app-server --stdio`, performs `initialize`/`initialized`, correlates bounded JSON-RPC requests, and shuts down its child safely. Keep protocol values private to the runtime module and expose only readiness, login, model discovery, and turn APIs needed by callers.

Add an optional serde-defaulted `codex_thread_id` to `AgentSession`, update fixtures, and add unit tests for framing, request correlation, executable selection, model parsing, login completion, turn status, and approval decisions. Do not read or expose Codex credentials.

## 2. Implement the OAuth turn path in the runtime

**Files:**

- `crates/medusa-runtime/src/lib.rs`
- `crates/medusa-runtime/src/support.rs`
- `crates/medusa-agent/src/session.rs` (event/session compatibility only)

Branch `run_prompt` only for `openai-oauth`. Create or resume the normal Medusa session, bind its Codex thread, send attachments in app-server input form, stream assistant deltas, persist the final assistant message/model metadata, and map terminal statuses to existing runtime events. Translate app-server command/file/permission requests into `AgentQuestion`, persist the question event, and resolve the stored JSON-RPC request on the next user answer. Send `turn/interrupt` on cancellation and fail closed on process/protocol loss or a stale approval.

Correct the settings/support credential summary so `auth=none` OAuth profiles are shown as Codex-managed rather than missing credentials.

Add runtime tests using a scripted JSONL app-server transport for session/thread persistence, streaming, cancellation, malformed input, and approval recovery.

## 3. Migrate configuration, CLI/TUI, and desktop startup

**Files:**

- `crates/medusa-config/src/openai_oauth.rs`
- `crates/medusa-config/src/provider_catalog.rs`
- `crates/medusa-config/src/provider_profile.rs`
- `crates/medusa-config/src/provider_profiles.rs`
- `crates/medusa-config/src/config_doctor.rs`
- `crates/medusa-cli/src/oauth_preflight.rs`
- `crates/medusa-cli/src/first_run.rs`
- `crates/medusa-cli/src/config_command.rs`
- `crates/medusa-cli/src/provider_diagnostic.rs`
- `apps/medusa-desktop/src-tauri/src/provider_auth.rs`

Remove production startup/probing of `127.0.0.1:10531`, `npx openai-oauth`, and gateway-specific remediation. Use the runtime app-server readiness/login/model-list APIs in CLI preflight, first-run setup, model discovery, config commands, and desktop auth commands. Keep compatibility command names only where the frontend contract requires them, but make their behavior and messages app-server based. Update OAuth catalog/profile validation and tests so no gateway URL is required.

## 4. Documentation and user-facing output

**Files:**

- `README.md`
- `docs/CHATGPT-OAUTH.md`
- `docs/PROVIDER-DELIVERY.md`
- `docs/architecture/shared-configuration-authority.md`
- `docs/testing/realtime-voice.md`

Describe Codex app-server ownership, executable prerequisites, browser login, model discovery, and the separate Realtime voice boundary. Remove claims that users must install or run the old gateway. Keep status output concise and actionable.

## 5. Verification and delivery

Run `cargo fmt --check`, focused package tests for config/agent/runtime/CLI/desktop, relevant TUI/daemon protocol tests, and `cargo clippy` for changed packages. Run the locked workspace suite with a bounded observation window and report any environmental timeout separately. Review the complete diff for gateway remnants, credential leakage, and error handling before committing.

Commit the implementation and documentation, push `main` without force, verify the remote commit, build the CLI release binary, build the Tauri desktop app using its standalone lockfile, and redeploy both artifacts locally. Verify the installed/local binaries report the new version and that app-server startup fails with a clear message when `codex` is unavailable.
