# Extended Reach and No-Truncation Display Design

## Goal

Give Medusa two related improvements:

1. A persistent headless browser the agent can drive from tool calls, so it can
   reach outside the sandboxed shell to interact with web applications.
2. A TUI display that shows the full text of every tool result, fetched page, and
   shell run, mirroring the inline-and-scroll behaviour of Claude Code, instead
   of the current truncated lines and `[truncated]` markers.

Both changes are scoped to a single implementation plan; neither depends on the
other, but they are designed together because they both affect the surface
between the agent's tool layer and the TUI.

The canonical source root for this design is
`Documents/Codex/2026-07-13/upd/work/medusa` on branch `main` at
`64da59db1897a1e4b0b17e5d6c84e4f530e03b69` (in sync with `origin/main`).

## Scope

In scope:

- A new `medusa-browserd` sidecar binary that wraps Playwright and exposes a
  small JSON protocol for browser actions.
- A new `medusa-browser-client` library crate used by the agent to talk to the
  sidecar.
- A `browser_*` tool set exposed to the agent: `browser_navigate`,
  `browser_snapshot`, `browser_click`, `browser_fill`, `browser_press`,
  `browser_screenshot`, `browser_evaluate`, `browser_tabs`, `browser_close`,
  `browser_ping`.
- A shared `output_envelope` helper used by `shell`, `web`, and `browser`
  tools: full body is persisted to a sidecar file under the session; a compact
  head/tail/line-count/path is sent to the model; the full body is streamed to
  the TUI.
- Removal of the width-limited `truncate` from the TUI renderer; long activity
  rows wrap on terminal width and the user can navigate them with
  `Shift+Up` / `Shift+Down` / `Shift+PgUp` / `Shift+PgDn` / `Home` / `End` /
  scrollbar, like Claude Code.
- Removal of the 1 MiB cap in `medusa-agent/src/tools/mod.rs::truncate`, the
  1 MiB cap in `medusa-daemon/src/lib.rs::truncate`, the 1 MiB cap in
  `medusa-workers/src/lib.rs::truncate`, and the 20 KiB char cap in
  `medusa-agent/src/tools/web.rs::truncate_text`.
- A configuration knob on the daemon and the agent to bound the on-disk sidecar
  size (default 256 MiB) and to disable the head/tail compaction per tool.

Out of scope (separate sub-projects):

- Network / IPC / MCP reach (`medusa-extensions/src/mcp.rs` already exists; this
  spec does not add new HTTP, socket, or MCP tools).
- Changes to the welcome screen, slash commands, modals, or status bar.
- Removing clipboard or file-attachment size limits; those protect memory and
  the model prompt, not the display.
- Visible changes to the question modal header compaction
  (`compact_question_header`, 12 chars) and plan-title compaction
  (`compact_plan_title`, 140 chars); both are header clamps, not display
  clamps, and are kept.

## Architecture

```
┌────────────────────────────┐         ┌────────────────────────────┐
│ medusa-tui (TUI)           │         │ medusa-agent (Rust)        │
│  ┌──────────────────────┐  │  ipc    │  ┌──────────────────────┐  │
│  │ AppState / renderer  │◀─┼─────────┼─▶│ engine.rs            │  │
│  │  • no more truncate() │  │  ws /  │  │  • tool router       │  │
│  │  • scrollback offset  │  │  sock  │  │  • envelope helper   │  │
│  │    Shift+Up / PgUp    │  │        │  │  • browser client    │  │
│  └──────────────────────┘  │        │  └──────────┬───────────┘  │
└────────────────────────────┘        │             │              │
                                      │      ┌──────┴────────┐     │
                                      │      │ browser socket │     │
                                      │      └──────┬────────┘     │
                                      │             │ JSON over   │
                                      │             │ stdio /     │
                                      │             │ named pipe  │
                                      │   ┌─────────▼──────────┐  │
                                      │   │ medusa-browserd     │  │
                                      │   │  • Playwright/Node   │  │
                                      │   │  • persistent ctx    │  │
                                      │   │  • 1 process / repo  │  │
                                      │   └─────────────────────┘  │
                                      └────────────────────────────┘
```

### New crates

- `crates/medusa-browserd/` — small CLI binary. Spawns Chromium via Playwright
  (the same Playwright stack the repo already uses in `browser/verify.mjs`),
  exposes a JSON-over-stdio protocol on Linux/macOS and a JSON-over-named-pipe
  protocol on Windows. One process per Medusa session. Cleaned up on agent
  shutdown or session resume. No state persists across sessions.

- `crates/medusa-browser-client/` — sync client for the sidecar. Defines the
  `BrowserRequest` / `BrowserResponse` types shared with `medusa-browserd` via a
  tiny JSON schema. Used by `medusa-agent`'s browser tools.

### Changed crates

- `crates/medusa-tui/src/lib.rs`
  - Remove the `truncate(&str, width) -> String` helper and every call site.
    `StyledLine::print` and `print_at` use terminal-width wrapping instead of
    ellipsis truncation.

- `crates/medusa-tui/src/app.rs`
  - Add `scrollback_offset: usize` to `AppState` (default 0).
  - Handle `Shift+Up` / `Shift+Down` / `Shift+PgUp` / `Shift+PgDn` / `Home` /
    `End` to adjust `scrollback_offset` against the total transcript height.
  - The transcript area becomes a virtual viewport over a flat list of styled
    lines; the existing `portable_render_snapshot` test must be updated to
    seed an 8 000-character activity and assert the rendered buffer contains
    every character, not just the first `width` characters.

- `crates/medusa-agent/src/tools/mod.rs`
  - Remove the public `truncate` helper. Replace with a private
    `apply_envelope` call used by `shell`, `web`, and `browser` tools.

- `crates/medusa-agent/src/tools/web.rs`
  - Remove `truncate_text` and the `MAX_TEXT_CHARS = 20_000` constant.
  - `fetch` returns the full readable text through the envelope helper.

- `crates/medusa-agent/src/tools/shell.rs`
  - `format_command_output` no longer calls `truncate`. The envelope helper
    handles persistence and compaction.

- `crates/medusa-agent/src/output_envelope.rs` (new)
  - Public type: `OutputEnvelope { head: String, tail: String, line_count: usize,
    byte_count: usize, path: PathBuf, format: OutputFormat }`.
  - `OutputFormat` is one of `Plain`, `JsonLines`, `Binary` (for screenshots —
    a sibling file `.png` is written next to the envelope metadata).
  - `wrap(repo: &Path, tool: &str, body: impl AsRef<[u8]>, ctx: &ToolContext)
    -> OutputEnvelope` — persists the body, computes head/tail, returns the
    envelope.
  - Head and tail sizes default to 4 KiB each; both are configurable per tool.

- `crates/medusa-agent/src/engine.rs`
  - The tool dispatcher in `dispatch_tool` calls `envelope::wrap` after every
    successful tool execution; the envelope is what the model sees.
  - The runtime continues to use the existing tool-result event type in
    `crates/medusa-agent/src/session.rs` to push the *full* body to the TUI.
    The plan will find the real type and rewire it if needed; this spec does
    not invent a new event name.

- `crates/medusa-daemon/src/lib.rs`
  - Remove the 1 MiB `truncate`. The daemon writes the full body to
    `<session>/artifacts/<job_id>.txt`; if the body exceeds
    `MEDUSA_DAEMON_MAX_ARTIFACT_BYTES` (default 256 MiB), the write is refused
    and the job records a `DependencyUnavailable` error with a clear message.
  - `MEDUSA_DAEMON_MAX_ARTIFACT_BYTES` is a new env var read at startup.

- `crates/medusa-workers/src/lib.rs`
  - Same change as the daemon. Same env var, same default.

- `crates/medusa-config/src/lib.rs`
  - Surface the new env var and the new agent-level knobs:
    - `medusa_browser_enabled` (bool, default `true` when `medusa-browserd`
      is on `PATH` or adjacent to the agent binary, otherwise `false`).
    - `medusa_browser_path` (optional absolute path to the `medusa-browserd`
      executable; if unset, the agent looks first in
      `<agent_dir>/medusa-browserd[.exe]` and then in `PATH`).
    - `medusa_browser_timeout_ms` (default 30 000).
    - `medusa_envelope_head_bytes` (default 4 096).
    - `medusa_envelope_tail_bytes` (default 4 096).
    - `MEDUSA_DAEMON_MAX_ARTIFACT_BYTES` (default 268 435 456 = 256 MiB).

### Browser protocol (medusa-browserd)

The sidecar speaks a tiny JSON request/response protocol. Every request has
the same shape:

```json
{ "id": "<ulid>", "method": "navigate", "params": { "url": "https://example.com" } }
```

Supported methods and their params:

- `ping` — `{}`. Returns `{ "ok": true, "version": "..." }`.
- `navigate` — `{ "url": "https://..." }`. Returns `{ "ok": true, "final_url": "...",
  "status": 200 }`.
- `snapshot` — `{}`. Returns `{ "ok": true, "text": "...", "refs": [{ "id": 1,
  "role": "button", "name": "Submit", "selector": "#submit" }] }`. The `text`
  is the visible text of the page; the `refs` are stable, opaque element
  references the agent can pass to `click` / `fill`.
- `click` — `{ "ref": 1 }` or `{ "selector": "#submit" }`. Returns
  `{ "ok": true, "navigated": false }`.
- `fill` — `{ "ref": 1, "value": "..." }` or `{ "selector": "...", "value":
  "..." }`. Returns `{ "ok": true }`.
- `press` — `{ "key": "Enter" }`. Returns `{ "ok": true }`.
- `screenshot` — `{ "full_page": false }`. Returns `{ "ok": true, "format":
  "png", "bytes": "<base64>" }`. The agent decodes and writes a sibling file
  under the envelope's path.
- `evaluate` — `{ "expression": "document.title" }`. Returns
  `{ "ok": true, "value": <json> }`.
- `tabs` — `{}`. Returns `{ "ok": true, "tabs": [{ "id": 0, "url": "...",
  "title": "..." }] }`.
- `close` — `{}`. Returns `{ "ok": true }` and exits the sidecar.

Errors: `{ "ok": false, "code": "navigation_failed", "message": "..." }`. The
`code` is a stable string the agent maps to a `MedusaError` category; the
`message` is human-readable and redacted by `medusa-extensions::redaction`
before being shown.

## Data flow

A single tool call, end to end:

```
tool call (e.g. browser_click { ref: 1 })
  └─ engine.rs::dispatch_tool
        └─ tools::browser::click(repo, input)
              └─ browser_client::request("click", { "ref": 1 })
                    │  JSON over stdio / named pipe
                    ▼
              medusa-browserd
                    │  Playwright API
                    ▼
              Chromium (CDP)
                    │
              ◀ BrowserResponse { ok: true, navigated: false }
              │
              └─ envelope::wrap(repo, "browser_click", body, &ctx)
                    │
                    ├──► <session>/artifacts/browser_click_<ulid>.txt
                    ├──► EngineResult { envelope: OutputEnvelope { ... } }
                    │       └─ engine sends the envelope to the model
                    └──► TuiEvent::ToolActivity { title, body }
                            └─ TUI renders the full body, no truncation
```

The same flow applies to `shell_run` and `web_fetch`; `web_search` keeps its
5-result cap (a semantic limit, not a display limit).

## Error handling

- `medusa-browserd` startup failure (no Node, no Chromium, wrong version):
  the agent returns a `DependencyUnavailable` error with a one-line
  remediation hint, and the rest of the tools keep working. The model is
  expected to fall back to `web_fetch` + `shell_run`.
- Browser context lost (page crash): the next call returns
  `Transient`; the engine restarts the sidecar on the following call and
  re-runs the action.
- Sidecar write failure (disk full, permission): the envelope helper falls
  back to returning the full body inline with a one-line warning; the TUI
  still shows everything.
- Network redirect to a non-public IP / disallowed port: the sidecar
  validates with the same rules as `validate_public_url` in
  `medusa-agent/src/tools/web.rs`; the rules are duplicated, not shared
  (avoids a `medusa-agent` dependency from `medusa-browserd`).
- Timeouts: 30 s default per browser call, configurable via
  `medusa_browser_timeout_ms` in `medusa-config`. The TUI shows a
  `● browser_click: waiting…` progress entry and replaces it with the
  result on completion.

## Testing

Unit (in `medusa-agent` and `medusa-browser-client`):

- Envelope round-trip for sizes 0, 1, 1 KiB, 1 MiB, 100 MiB. Assert
  `head` + `tail` size, `line_count`, UTF-8 char boundaries, and that
  `path` exists and round-trips.
- Browser protocol: every method has a fake-server test that returns canned
  JSON and verifies the client maps it to the correct Rust result. Errors
  map to the right `MedusaError` category.
- Head/tail logic: ensure UTF-8 boundaries, `line_count` is exact, no
  double-encoding.

Integration (in `medusa-browserd`, gated on Playwright + Chromium):

- `navigate → snapshot → click → assert DOM change → screenshot → diff with
  a stored PNG`. Marked `#[ignore = "requires Playwright + Chromium"]` so
  it does not break the default CI; run in the `release-gates` workflow.
- The fixture HTML in `browser/fixture/` is reused.

End-to-end:

- Drive a real `medusa-browserd` from a scripted agent loop
  (`browser/e2e.mjs` or a new Rust harness). Assert:
  - The TUI shows the full 5 MiB stdout without `[truncated]`.
  - The model sees the head/tail/path envelope and not the full body.
- Drive the TUI in a real terminal with a 5 MiB activity row and assert
  the user can scroll back to read it.

TUI:

- The existing `portable_render_snapshot` test is replaced with one that
  seeds an 8 000-character activity and asserts the rendered buffer
  contains every character.
- New tests for `Shift+Up` / `Shift+PgUp` adjusting `scrollback_offset`.

Regression:

- Re-run `medusa-hardening` and `medusa-improvement` benchmarks. Expect
  per-turn tool-result bytes to grow (because full bodies are stored) and
  per-turn model input tokens to *drop* (because the envelope head/tail is
  smaller than the previous 1 MiB blob the model used to see).

## Validation

- Spec self-review (this section) — no TBDs, no contradictions, scope is
  one plan, no ambiguity in the tool protocol or the envelope shape.
- User review of the written spec at
  `docs/superpowers/specs/2026-07-14-medusa-extended-reach-and-no-truncation-design.md`.
- `superpowers:writing-plans` produces a step-by-step implementation plan
  that is reviewed in turn before any code is written.

## Alternatives Rejected

- **Per-call Playwright child + lazy pager in the TUI.** Cheaper to build,
  but every browser call is a fresh page; a "log in, click X, screenshot"
  sequence is impossible without re-navigating each time. The lazy pager
  also re-introduces hidden bodies behind a keypress, which contradicts
  the "no truncation" goal.
- **Embedded `chromiumoxide` + zero truncation everywhere.** Fewest moving
  parts, but ignores the existing `browser/` Playwright fixtures, and
  passing 100 MiB of HTML to the model on every follow-up turn is a real
  cost. The envelope pattern keeps the model context small while the TUI
  shows the full body.
- **Removing the head/tail compaction and sending the full body to the
  model.** Simpler, but blows up the model's context window and token
  cost. The envelope is the right trade-off.
- **Removing the 12-char `compact_question_header` and 140-char
  `compact_plan_title` clamps.** Those are *header* clamps that protect
  the question modal and plan UI; the user asked for the TUI to stop
  truncating *content*, and these are not content. Left in place.

## Open Questions

None. All clarifying questions have been answered:

- Reach focus: browser / web interaction.
- Display fix: full content, inline / scroll, no truncation, like Claude Code.
- Browser scope: headless browser driving.
- Browser runtime: sidecar process in Rust.
- Approach: A (Playwright sidecar, persistent context, full-text tool
  returns).
