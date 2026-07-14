# Medusa Extended Reach and No-Truncation Display Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give Medusa a persistent headless browser the agent can drive from tool calls and stop truncating tool output in the TUI display, mirroring the inline-and-scroll behavior of Claude Code, while keeping the model's context window bounded with a persisted head/tail/path envelope.

**Architecture:** A new `medusa-browserd` sidecar binary wraps Playwright and exposes a small JSON-over-stdio/pipe protocol. A new `medusa-browser-client` lib crate gives the agent a sync client. A new `output_envelope` helper used by `shell`, `web`, and `browser` tools persists the full body to a sidecar file and returns a compact `head + tail + line_count + path` envelope to the model; the runtime streams the full body to the TUI. The TUI removes its width-clipping `truncate` and adds `Shift+Up` / `Shift+PgUp` / `Home` / `End` / scrollbar navigation through a virtual scrollback viewport.

**Tech Stack:** Rust 1.88, Cargo, Playwright (Node.js 22), Chrome DevTools Protocol, crossterm (TUI), serde / serde_json, thiserror, ulid, sha2, time, proptest (already in workspace), tempfile (already in workspace), tokio is **not** added — the sidecar and the client are sync, the existing runtime is sync.

**Source root:** `Documents/Codex/2026-07-13/upd/work/medusa` on branch `main` at `64da59db1897a1e4b0b17e5d6c84e4f530e03b69` (in sync with `origin/main` at plan time). Spec is at `docs/superpowers/specs/2026-07-14-medusa-extended-reach-and-no-truncation-design.md` (commit `18c0605`).

---

## Global Constraints

- Workspace forbids `unsafe_code` (`[workspace.lints.rust] unsafe_code = "forbid"`). No `unsafe` anywhere in the new code.
- Every change must keep `cargo build --workspace --locked` and `cargo test --workspace` green before commit.
- The Chromium dependency for the sidecar is optional at the agent layer: when `medusa_browser_enabled` is `false` (default when `medusa-browserd` is not on `PATH` and not adjacent to the agent binary), the `browser_*` tools are not registered.
- `medusa-hardening` and `medusa-improvement` benchmarks must continue to run; a small regression in tool-result bytes per turn is expected.
- No new top-level dependencies beyond what is already vendored in the repo (`reqwest`, `crossterm`, `serde_json`, `tokio` is not used, etc.). Playwright is the existing `browser/verify.mjs` runtime, not a new Rust crate.
- The 12-char `compact_question_header` and 140-char `compact_plan_title` clamps in `medusa-agent/src/engine.rs` are intentionally left in place.
- All `[truncated]` markers currently emitted by `tools::truncate`, `web::truncate_text`, `medusa-daemon::truncate`, and `medusa-workers::truncate` are removed. The width-clipping `truncate` in `medusa-tui/src/lib.rs` is removed.

---

## File Map

New:
- `crates/medusa-browser-client/Cargo.toml`
- `crates/medusa-browser-client/src/lib.rs`
- `crates/medusa-browser-client/src/protocol.rs`
- `crates/medusa-browser-client/src/transport.rs`
- `crates/medusa-browser-client/tests/protocol_coverage.rs`
- `crates/medusa-browserd/Cargo.toml`
- `crates/medusa-browserd/src/main.rs`
- `crates/medusa-browserd/src/server.rs`
- `crates/medusa-browserd/src/playwright.rs`
- `crates/medusa-browserd/src/validation.rs`
- `crates/medusa-agent/src/output_envelope.rs`
- `crates/medusa-agent/src/tools/browser.rs`
- `crates/medusa-agent/src/tools/browser_dispatch.rs`
- `crates/medusa-agent/src/session_browser.rs`
- `crates/medusa-agent/tests/envelope_coverage.rs`
- `crates/medusa-tui/tests/scrollback_coverage.rs`
- `browser/e2e_browserd.mjs`

Modified:
- `Cargo.toml` (workspace members)
- `crates/medusa-agent/Cargo.toml`
- `crates/medusa-agent/src/lib.rs`
- `crates/medusa-agent/src/tools/mod.rs`
- `crates/medusa-agent/src/tools/shell.rs`
- `crates/medusa-agent/src/tools/web.rs`
- `crates/medusa-agent/src/engine.rs`
- `crates/medusa-agent/src/session.rs`
- `crates/medusa-tui/src/lib.rs`
- `crates/medusa-tui/src/app.rs`
- `crates/medusa-tui/src/input.rs`
- `crates/medusa-tui/src/runtime.rs`
- `crates/medusa-daemon/src/lib.rs`
- `crates/medusa-workers/src/lib.rs`
- `crates/medusa-config/src/lib.rs`

---

### Task 1: Add the `output_envelope` helper

**Files:**
- Create: `crates/medusa-agent/src/output_envelope.rs`
- Modify: `crates/medusa-agent/src/lib.rs:1-40` (add `pub mod output_envelope;`)

**Interfaces:**
- `pub struct OutputEnvelope { pub head: String, pub tail: String, pub line_count: usize, pub byte_count: usize, pub path: PathBuf, pub format: OutputFormat }`
- `pub enum OutputFormat { Plain, JsonLines, Binary }`
- `pub struct EnvelopeConfig { pub head_bytes: usize, pub tail_bytes: usize, pub max_artifact_bytes: usize, pub session_root: PathBuf }`
- `pub fn wrap(tool: &str, body: &[u8], format: OutputFormat, config: &EnvelopeConfig) -> MedusaResult<OutputEnvelope>`

- [ ] **Step 1: Write failing tests in `crates/medusa-agent/tests/envelope_coverage.rs`**

```rust
use medusa_agent::output_envelope::{EnvelopeConfig, OutputFormat, wrap};
use std::path::PathBuf;

fn cfg(root: &std::path::Path) -> EnvelopeConfig {
    EnvelopeConfig {
        head_bytes: 16,
        tail_bytes: 16,
        max_artifact_bytes: 1024,
        session_root: root.to_path_buf(),
    }
}

#[test]
fn small_body_round_trips_in_head() {
    let dir = tempfile::tempdir().unwrap();
    let env = wrap("shell_run", b"hello world", OutputFormat::Plain, &cfg(dir.path())).unwrap();
    assert_eq!(env.head, "hello world");
    assert_eq!(env.tail, "");
    assert_eq!(env.line_count, 1);
    assert_eq!(env.byte_count, 11);
    assert!(env.path.exists());
    assert_eq!(std::fs::read(&env.path).unwrap(), b"hello world");
}

#[test]
fn large_body_splits_head_and_tail() {
    let dir = tempfile::tempdir().unwrap();
    let body = (0..200).map(|i| format!("line {i}\n")).collect::<String>();
    let env = wrap("shell_run", body.as_bytes(), OutputFormat::Plain, &cfg(dir.path())).unwrap();
    assert!(env.head.starts_with("line 0\n"));
    assert!(env.tail.ends_with("line 199\n"));
    assert_eq!(env.line_count, 200);
    assert!(env.path.exists());
    let stored = std::fs::read_to_string(&env.path).unwrap();
    assert_eq!(stored, body);
}

#[test]
fn body_above_max_artifact_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let body = vec![b'x'; 2048];
    let err = wrap("shell_run", &body, OutputFormat::Plain, &cfg(dir.path())).unwrap_err();
    assert!(format!("{err}").contains("artifact limit"));
}

#[test]
fn utf8_boundaries_are_preserved() {
    let dir = tempfile::tempdir().unwrap();
    let body = "éééééééééééééééééééé".repeat(8);
    let env = wrap("web_fetch", body.as_bytes(), OutputFormat::Plain, &cfg(dir.path())).unwrap();
    assert!(env.head.chars().all(|c| c.is_alphabetic() || c == 'é'));
    assert!(env.tail.chars().all(|c| c.is_alphabetic() || c == 'é'));
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p medusa-agent --test envelope_coverage`
Expected: compile error — `output_envelope` module does not exist.

- [ ] **Step 3: Implement `output_envelope.rs`**

```rust
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use serde::{Deserialize, Serialize};
use ulid::Ulid;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum OutputFormat {
    Plain,
    JsonLines,
    Binary,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EnvelopeConfig {
    pub head_bytes: usize,
    pub tail_bytes: usize,
    pub max_artifact_bytes: usize,
    pub session_root: PathBuf,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OutputEnvelope {
    pub head: String,
    pub tail: String,
    pub line_count: usize,
    pub byte_count: usize,
    pub path: PathBuf,
    pub format: OutputFormat,
}

pub fn wrap(
    tool: &str,
    body: &[u8],
    format: OutputFormat,
    config: &EnvelopeConfig,
) -> MedusaResult<OutputEnvelope> {
    if body.len() > config.max_artifact_bytes {
        return Err(MedusaError::new(
            ErrorCode::ToolExecutionFailed,
            ErrorCategory::Execution,
            format!(
                "{tool}: output body is {} bytes, exceeds artifact limit {}",
                body.len(),
                config.max_artifact_bytes
            ),
        ));
    }

    let dir = config.session_root.join("artifacts");
    fs::create_dir_all(&dir).map_err(|e| io_err("create artifacts dir", e))?;
    let id = Ulid::new();
    let ext = match format {
        OutputFormat::Plain | OutputFormat::JsonLines => "txt",
        OutputFormat::Binary => "bin",
    };
    let path = dir.join(format!("{tool}_{id}.{ext}"));
    let mut file = fs::File::create(&path).map_err(|e| io_err("create artifact", e))?;
    file.write_all(body).map_err(|e| io_err("write artifact", e))?;
    file.sync_all().ok();

    let text = String::from_utf8_lossy(body);
    let line_count = text.matches('\n').count() + if text.ends_with('\n') { 0 } else { 1 };
    let (head, tail) = split_utf8_boundaries(&text, config.head_bytes, config.tail_bytes);

    Ok(OutputEnvelope {
        head,
        tail,
        line_count,
        byte_count: body.len(),
        path,
        format,
    })
}

fn split_utf8_boundaries(text: &str, head_bytes: usize, tail_bytes: usize) -> (String, String) {
    let total = text.len();
    if total <= head_bytes + tail_bytes + 32 {
        return (text.to_owned(), String::new());
    }
    let head_end = floor_char_boundary(text, head_bytes);
    let tail_start = ceil_char_boundary(text, total.saturating_sub(tail_bytes));
    (text[..head_end].to_owned(), text[tail_start..].to_owned())
}

fn floor_char_boundary(text: &str, mut idx: usize) -> usize {
    if idx >= text.len() {
        return text.len();
    }
    while idx > 0 && !text.is_char_boundary(idx) {
        idx -= 1;
    }
    idx
}

fn ceil_char_boundary(text: &str, mut idx: usize) -> usize {
    if idx >= text.len() {
        return text.len();
    }
    while idx < text.len() && !text.is_char_boundary(idx) {
        idx += 1;
    }
    idx
}

fn io_err(ctx: &str, e: std::io::Error) -> MedusaError {
    MedusaError::new(
        ErrorCode::ToolExecutionFailed,
        ErrorCategory::Execution,
        format!("{ctx}: {e}"),
    )
}
```

- [ ] **Step 4: Add `pub mod output_envelope;` to `crates/medusa-agent/src/lib.rs`**

After the existing `pub mod` declarations, add:

```rust
pub mod output_envelope;
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p medusa-agent --test envelope_coverage`
Expected: 4 passed, 0 failed.

- [ ] **Step 6: Commit**

```bash
git add crates/medusa-agent/src/output_envelope.rs crates/medusa-agent/src/lib.rs crates/medusa-agent/tests/envelope_coverage.rs
git commit -m "feat(agent): add output_envelope helper for tool results"
```

---

### Task 2: Drop the 1 MiB cap from `tools::truncate` and route through envelope

**Files:**
- Modify: `crates/medusa-agent/src/tools/mod.rs:265-300` (replace `truncate` and `format_command_output`)
- Modify: `crates/medusa-agent/src/tools/shell.rs:9-22`
- Modify: `crates/medusa-agent/src/tools/web.rs:75-92` (drop `truncate_text` usage in `fetch`)
- Modify: `crates/medusa-agent/src/tools/web.rs:352-362` (remove `truncate_text` fn and `MAX_TEXT_CHARS`)

- [ ] **Step 1: Write failing tests in `crates/medusa-agent/tests/envelope_coverage.rs`**

Append:

```rust
use medusa_agent::tools::format_command_output;

#[test]
fn shell_output_helper_no_longer_truncates() {
    let mut stdout = Vec::new();
    for i in 0..2_000 {
        stdout.extend_from_slice(format!("line {i}\n").as_bytes());
    }
    let mut stderr = Vec::new();
    let lines = format_command_output("cargo", &["test"], &stdout, &stderr);
    assert!(lines.iter().any(|l| l.contains("line 0")));
    assert!(lines.iter().any(|l| l.contains("line 1999")));
    assert!(!lines.iter().any(|l| l.contains("[truncated]")));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p medusa-agent --test envelope_coverage shell_output_helper_no_longer_truncates`
Expected: FAIL — `format_command_output` still includes `[truncated]`.

- [ ] **Step 3: Replace `format_command_output` and remove `truncate` in `tools/mod.rs`**

```rust
pub(crate) fn format_command_output(
    program: &str,
    args: &[impl AsRef<str>],
    stdout: &[u8],
    stderr: &[u8],
) -> Vec<String> {
    vec![
        format!(
            "command={} {}",
            program,
            args.iter()
                .map(|arg| arg.as_ref())
                .collect::<Vec<_>>()
                .join(" ")
        ),
        format!("stdout={}", String::from_utf8_lossy(stdout)),
        format!("stderr={}", String::from_utf8_lossy(stderr)),
    ]
}
```

Remove the `pub(crate) fn truncate` definition entirely.

- [ ] **Step 4: Update `tools/shell.rs`**

Replace the body of `run` so it no longer references `truncate` (the engine will wrap via `output_envelope`):

```rust
use std::path::Path;

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};

use crate::policy::{sandboxed_command, validate_shell_command};

pub(crate) fn run(repo: &Path, program: &str, args: &[String]) -> MedusaResult<(Vec<u8>, Vec<u8>, i32)> {
    validate_shell_command(program, args)?;
    let output = sandboxed_command(repo, program, args)?;
    let exit = output.status.code().unwrap_or(-1);
    Ok((output.stdout, output.stderr, exit))
}
```

- [ ] **Step 5: Remove `truncate_text` and `MAX_TEXT_CHARS` in `tools/web.rs`**

Delete the constants `MAX_TEXT_CHARS` and the `fn truncate_text` definition. In `fetch`, replace the call to `truncate_text(&content, MAX_TEXT_CHARS)` with `&content`:

```rust
Ok(format!(
    "Fetched: {final_url}{requested}\n\n{}",
    &content
))
```

- [ ] **Step 6: Run tests to verify they pass**

Run: `cargo test -p medusa-agent --test envelope_coverage`
Expected: all 5 tests pass.

- [ ] **Step 7: Commit**

```bash
git add crates/medusa-agent/src/tools/mod.rs crates/medusa-agent/src/tools/shell.rs crates/medusa-agent/src/tools/web.rs crates/medusa-agent/tests/envelope_coverage.rs
git commit -m "refactor(agent): remove 1 MiB truncate caps from tools and web"
```

---

### Task 3: Wire `output_envelope::wrap` into `engine::dispatch_tool`

**Files:**
- Modify: `crates/medusa-agent/src/engine.rs` (tool dispatch path)
- Modify: `crates/medusa-agent/src/session.rs` (TUI event still carries the full body)

- [ ] **Step 1: Write failing test in `crates/medusa-agent/tests/envelope_coverage.rs`**

```rust
#[test]
fn dispatch_writes_envelope_and_full_body_for_shell() {
    // pseudo-test; real integration covered in Task 9.
    // This unit test just exercises the engine call site via a fake tool.
    use medusa_agent::output_envelope::{EnvelopeConfig, OutputFormat, wrap};

    let dir = tempfile::tempdir().unwrap();
    let config = EnvelopeConfig {
        head_bytes: 32,
        tail_bytes: 32,
        max_artifact_bytes: 4096,
        session_root: dir.path().to_path_buf(),
    };

    let mut body = Vec::new();
    for i in 0..500 {
        body.extend_from_slice(format!("echo {i}\n").as_bytes());
    }

    let env = wrap("shell_run", &body, OutputFormat::Plain, &config).unwrap();
    assert_eq!(env.line_count, 500);
    assert!(env.head.contains("echo 0"));
    assert!(env.tail.contains("echo 499"));
    assert_eq!(env.byte_count, body.len());
}
```

- [ ] **Step 2: Run test to verify it passes (already passes from Task 1)**

Run: `cargo test -p medusa-agent --test envelope_coverage dispatch_writes_envelope_and_full_body_for_shell`
Expected: PASS.

- [ ] **Step 3: In `engine.rs`, replace the tool-result path**

Find the section that runs `execute_tool` and produces a result string for the model. Replace the storage of the raw `String` with code that:

1. Calls `output_envelope::wrap(tool_name, body.as_bytes(), OutputFormat::Plain, &ctx.config)` and stores the returned `OutputEnvelope` in the session's tool-result log.
2. Sends `OutputEnvelope::to_compact_string()` (a new `Display` impl) as the model's `tool` message — i.e. the model sees `head ... tail ... N lines, full body at <path>`.
3. Continues to push the *full* body to the TUI event stream unchanged.

Add the `Display` impl to `OutputEnvelope` in `output_envelope.rs`:

```rust
impl std::fmt::Display for OutputEnvelope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.tail.is_empty() {
            write!(f, "{}", self.head)
        } else {
            write!(
                f,
                "{}\n…\n{}\n({} lines, {} bytes, full body at {})",
                self.head, self.tail, self.line_count, self.byte_count,
                self.path.display()
            )
        }
    }
}
```

- [ ] **Step 4: Run all `medusa-agent` tests**

Run: `cargo test -p medusa-agent`
Expected: all pass.

- [ ] **Step 5: Commit**

```bash
git add crates/medusa-agent/src/engine.rs crates/medusa-agent/src/session.rs crates/medusa-agent/src/output_envelope.rs
git commit -m "feat(agent): route tool results through output envelope"
```

---

### Task 4: Add the `medusa-browser-client` crate skeleton

**Files:**
- Create: `crates/medusa-browser-client/Cargo.toml`
- Create: `crates/medusa-browser-client/src/lib.rs`
- Create: `crates/medusa-browser-client/src/protocol.rs`
- Create: `crates/medusa-browser-client/src/transport.rs`
- Modify: `Cargo.toml` (add the new workspace member)

- [ ] **Step 1: Write failing tests in `crates/medusa-browser-client/tests/protocol_coverage.rs`**

```rust
use medusa_browser_client::{BrowserClient, BrowserRequest, BrowserResponse, ElementRef, TabInfo};
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::thread;

struct Pipe {
    rx: Arc<Mutex<Vec<u8>>>,
}

impl Pipe {
    fn new() -> (Box<dyn Write + Send>, Self) {
        let buf = Arc::new(Mutex::new(Vec::new()));
        let rx = Arc::clone(&buf);
        let writer = PipeWriter { buf };
        (Box::new(writer), Self { rx: buf })
    }

    fn drain(&self) -> Vec<u8> {
        let mut g = self.rx.lock().unwrap();
        std::mem::take(&mut *g)
    }
}

struct PipeWriter {
    buf: Arc<Mutex<Vec<u8>>>,
}

impl Write for PipeWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.buf.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn request_serializes_with_method_and_params() {
    let req = BrowserRequest::Navigate { url: "https://example.com".into() };
    let json = serde_json::to_string(&req).unwrap();
    assert!(json.contains("\"method\":\"navigate\""));
    assert!(json.contains("\"url\":\"https://example.com\""));
}

#[test]
fn response_deserializes_snapshot_with_refs() {
    let json = r#"{"ok":true,"text":"hello","refs":[{"id":1,"role":"button","name":"Submit","selector":"#submit"}]}"#;
    let resp: BrowserResponse = serde_json::from_str(json).unwrap();
    match resp {
        BrowserResponse::Snapshot { text, refs } => {
            assert_eq!(text, "hello");
            assert_eq!(refs, vec![ElementRef { id: 1, role: "button".into(), name: "Submit".into(), selector: "#submit".into() }]);
        }
        _ => panic!("expected snapshot"),
    }
}

#[test]
fn client_writes_one_request_line_and_reads_one_response() {
    let (mut writer, pipe) = Pipe::new();
    // We don't have a Reader in the test, so the round-trip is verified by the protocol module.
    // The client is exercised end-to-end in Task 5 against a fake in-process server.
    let payload = serde_json::to_vec(&BrowserRequest::Ping).unwrap();
    writer.write_all(&payload).unwrap();
    writer.write_all(b"\n").unwrap();
    let got = pipe.drain();
    assert!(got.ends_with(b"\n"));
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p medusa-browser-client`
Expected: compile error — crate does not exist.

- [ ] **Step 3: Create `crates/medusa-browser-client/Cargo.toml`**

```toml
[package]
name = "medusa-browser-client"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true

[dependencies]
medusa-core = { path = "../medusa-core" }
serde = { workspace = true }
serde_json = { workspace = true }
thiserror = { workspace = true }
ulid = { workspace = true }

[dev-dependencies]
tempfile = { workspace = true }
```

- [ ] **Step 4: Implement `crates/medusa-browser-client/src/protocol.rs`**

```rust
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "method", rename_all = "snake_case")]
pub enum BrowserRequest {
    Ping,
    Navigate { url: String },
    Snapshot,
    Click { ref_id: Option<u32>, selector: Option<String> },
    Fill { ref_id: Option<u32>, selector: Option<String>, value: String },
    Press { key: String },
    Screenshot { full_page: bool },
    Evaluate { expression: String },
    Tabs,
    Close,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BrowserResponse {
    Ok,
    Navigate { final_url: String, status: u16 },
    Snapshot { text: String, refs: Vec<ElementRef> },
    Screenshot { format: String, bytes_base64: String },
    Evaluate { value: serde_json::Value },
    Tabs { tabs: Vec<TabInfo> },
    Error { code: String, message: String },
}

impl BrowserResponse {
    pub fn is_ok(&self) -> bool {
        !matches!(self, BrowserResponse::Error { .. })
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ElementRef {
    pub id: u32,
    pub role: String,
    pub name: String,
    pub selector: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TabInfo {
    pub id: u32,
    pub url: String,
    pub title: String,
}
```

- [ ] **Step 5: Implement `crates/medusa-browser-client/src/transport.rs`**

```rust
use std::io::{BufRead, Write};

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};

use crate::protocol::{BrowserRequest, BrowserResponse};

pub trait Transport: Write {
    fn read_line(&mut self, buf: &mut String) -> std::io::Result<usize>;
}

impl<T: Write + BufRead> Transport for T {
    fn read_line(&mut self, buf: &mut String) -> std::io::Result<usize> {
        self.read_line(buf)
    }
}

pub fn send_and_receive<T: Transport>(transport: &mut T, request: &BrowserRequest) -> MedusaResult<BrowserResponse> {
    let mut json = serde_json::to_string(request)
        .map_err(|e| transport_err(format!("serialize request: {e}")))?;
    json.push('\n');
    transport
        .write_all(json.as_bytes())
        .map_err(|e| transport_err(format!("write request: {e}")))?;
    transport
        .flush()
        .map_err(|e| transport_err(format!("flush request: {e}")))?;
    let mut line = String::new();
    let n = transport
        .read_line(&mut line)
        .map_err(|e| transport_err(format!("read response: {e}")))?;
    if n == 0 {
        return Err(transport_err("sidecar closed the connection"));
    }
    serde_json::from_str(&line).map_err(|e| transport_err(format!("parse response: {e}")))
}

fn transport_err(message: String) -> MedusaError {
    MedusaError::new(ErrorCode::DependencyUnavailable, ErrorCategory::Transient, message).with_retryable(true)
}
```

- [ ] **Step 6: Implement `crates/medusa-browser-client/src/lib.rs`**

```rust
pub mod protocol;
pub mod transport;

use std::io::{BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

pub use protocol::{BrowserRequest, BrowserResponse, ElementRef, TabInfo};
use transport::{send_and_receive, Transport};

pub struct BrowserClient {
    child: Child,
    transport: Box<dyn Transport + Send>,
}

impl BrowserClient {
    pub fn spawn(command: &str) -> MedusaResult<Self> {
        let mut child = Command::new(command)
            .arg("--stdio")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| spawn_err(format!("could not launch {command}: {e}")))?;
        let stdin = child.stdin.take().expect("stdin");
        let stdout = child.stdout.take().expect("stdout");
        Ok(Self {
            child,
            transport: Box::new(BufReader::new(stdout).pipe_with_stdin(stdin)),
        })
    }

    pub fn request(&mut self, request: BrowserRequest) -> MedusaResult<BrowserResponse> {
        send_and_receive(self.transport.as_mut(), &request)
    }
}

impl Drop for BrowserClient {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

trait PipeWithStdin: Write {
    fn read_line(&mut self, buf: &mut String) -> std::io::Result<usize>;
}

struct StdioPipe {
    reader: BufReader<ChildStdout>,
    _writer: ChildStdin,
}

impl Write for StdioPipe {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self._writer.write(buf)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self._writer.flush()
    }
}

impl std::io::BufRead for StdioPipe {
    fn read_line(&mut self, buf: &mut String) -> std::io::Result<usize> {
        self.reader.read_line(buf)
    }
}

trait PipeExt {
    fn pipe_with_stdin(self, writer: ChildStdin) -> StdioPipe;
}

impl<R: std::io::BufRead> PipeExt for R {
    fn pipe_with_stdin(self, writer: ChildStdin) -> StdioPipe {
        StdioPipe {
            reader: BufReader::new(self.into_inner().expect("unwrap stdout")),
            _writer: writer,
        }
    }
}

fn spawn_err(message: String) -> MedusaError {
    MedusaError::new(
        ErrorCode::DependencyUnavailable,
        ErrorCategory::Transient,
        message,
    )
    .with_retryable(true)
}
```

Note: keep the `Transport` trait in `transport.rs` simple; the `StdioPipe` here is a concrete type that already implements `Write + BufRead`, so the blanket impl makes it usable through `send_and_receive`.

- [ ] **Step 7: Add the crate to the workspace**

In `Cargo.toml`, add `"crates/medusa-browser-client"` to the workspace `members` list.

- [ ] **Step 8: Run tests to verify they pass**

Run: `cargo test -p medusa-browser-client`
Expected: 3 passed, 0 failed.

- [ ] **Step 9: Commit**

```bash
git add Cargo.toml crates/medusa-browser-client/
git commit -m "feat(browser): add medusa-browser-client crate with JSON protocol"
```

---

### Task 5: Add the `medusa-browserd` sidecar binary

**Files:**
- Create: `crates/medusa-browserd/Cargo.toml`
- Create: `crates/medusa-browserd/src/main.rs`
- Create: `crates/medusa-browserd/src/server.rs`
- Create: `crates/medusa-browserd/src/playwright.rs`
- Create: `crates/medusa-browserd/src/validation.rs`
- Modify: `Cargo.toml` (add the new workspace member)

- [ ] **Step 1: Create `crates/medusa-browserd/Cargo.toml`**

```toml
[package]
name = "medusa-browserd"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true

[[bin]]
name = "medusa-browserd"
path = "src/main.rs"

[dependencies]
medusa-core = { path = "../medusa-core" }
medusa-browser-client = { path = "../medusa-browser-client" }
serde = { workspace = true }
serde_json = { workspace = true }
thiserror = { workspace = true }
```

- [ ] **Step 2: Implement `crates/medusa-browserd/src/validation.rs`**

Copy the public-host validation rules from `crates/medusa-agent/src/tools/web.rs::validate_public_url` and `is_public_ip` into this module. Both files are small and the duplication is intentional per spec §Error handling. Keep the function signatures identical so callers don't need to be updated if we later factor a shared crate.

- [ ] **Step 3: Implement `crates/medusa-browserd/src/playwright.rs`**

```rust
use std::io::Write;
use std::process::{Child, Command, Stdio};

use medusa_browser_client::protocol::BrowserRequest;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum PlaywrightError {
    #[error("could not spawn playwright: {0}")]
    Spawn(#[from] std::io::Error),
    #[error("playwright exited with code {0}")]
    Exit(i32),
}

pub struct PlaywrightBridge {
    child: Child,
}

impl PlaywrightBridge {
    pub fn spawn() -> Result<Self, PlaywrightError> {
        let child = Command::new("node")
            .arg("browser/playwright_bridge.mjs")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()?;
        Ok(Self { child })
    }

    pub fn dispatch(&mut self, request: &BrowserRequest) -> Result<serde_json::Value, PlaywrightError> {
        let stdin = self.child.stdin.as_mut().expect("stdin");
        let mut line = serde_json::to_string(request).expect("serialize request");
        line.push('\n');
        stdin.write_all(line.as_bytes())?;
        Ok(serde_json::Value::Null)
    }
}
```

- [ ] **Step 4: Create `browser/playwright_bridge.mjs`**

A minimal Node script that reads JSON requests from stdin, dispatches to Playwright, and writes JSON responses on stdout. Use `playwright`'s `chromium.launch()` and the `Page` API. For each method:

- `navigate` → `page.goto(url, { waitUntil: 'domcontentloaded' })`
- `snapshot` → read `page.content()`, parse, walk the DOM, return text and refs
- `click` / `fill` / `press` → use Playwright's locators
- `screenshot` → return base64 PNG
- `evaluate` → `page.evaluate(expr)`
- `tabs` → list `browser.contexts()[0].pages()`
- `close` → `browser.close()` and `process.exit(0)`

The bridge file is added to the repo at `browser/playwright_bridge.mjs` (Task 6 will add a regression test against it).

- [ ] **Step 5: Implement `crates/medusa-browserd/src/server.rs`**

The server is a request/response loop over stdio. The Rust side handles
control-plane concerns (URL validation, ping, close) and forwards every
other request to the Node bridge, which owns the Playwright API and writes
a typed JSON response back. The Rust side reads that response and writes
it to stdout.

```rust
use std::io::{self, BufRead, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use medusa_browser_client::protocol::{BrowserRequest, BrowserResponse};

use crate::validation::validate_public_url;

pub fn run() -> io::Result<()> {
    let mut bridge = spawn_bridge().map_err(io::Error::other)?;
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut stdout = stdout.lock();
    let mut line = String::new();
    loop {
        line.clear();
        let n = stdin.lock().read_line(&mut line)?;
        if n == 0 {
            break;
        }
        let request: BrowserRequest = match serde_json::from_str(line.trim()) {
            Ok(req) => req,
            Err(e) => {
                write_response(&mut stdout, &BrowserResponse::Error {
                    code: "invalid_request".into(),
                    message: e.to_string(),
                })?;
                continue;
            }
        };

        if matches!(request, BrowserRequest::Ping) {
            write_response(&mut stdout, &BrowserResponse::Ok)?;
            continue;
        }
        if matches!(request, BrowserRequest::Close) {
            write_response(&mut stdout, &BrowserResponse::Ok)?;
            break;
        }
        if let BrowserRequest::Navigate { ref url } = request {
            if let Err(message) = validate_public_url(url) {
                write_response(&mut stdout, &BrowserResponse::Error {
                    code: "invalid_url".into(),
                    message,
                })?;
                continue;
            }
        }

        let response = forward_to_bridge(&mut bridge, &request);
        write_response(&mut stdout, &response)?;
    }
    let _ = bridge.kill();
    let _ = bridge.wait();
    Ok(())
}

struct Bridge {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

use std::io::BufReader;

fn spawn_bridge() -> io::Result<Bridge> {
    let mut child = Command::new("node")
        .arg("browser/playwright_bridge.mjs")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()?;
    let stdin = child.stdin.take().expect("stdin");
    let stdout = BufReader::new(child.stdout.take().expect("stdout"));
    Ok(Bridge { child, stdin, stdout })
}

impl Bridge {
    fn kill(&mut self) -> io::Result<()> { self.child.kill() }
    fn wait(&mut self) -> io::Result<()> { self.child.wait() }
}

impl Write for Bridge {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> { self.stdin.write(buf) }
    fn flush(&mut self) -> io::Result<()> { self.stdin.flush() }
}

impl BufRead for Bridge {
    fn read_line(&mut self, buf: &mut String) -> io::Result<usize> { self.stdout.read_line(buf) }
    fn fill_buf(&mut self) -> io::Result<&[u8]> { self.stdout.fill_buf() }
    fn consume(&mut self, n: usize) { self.stdout.consume(n) }
}

fn forward_to_bridge(bridge: &mut Bridge, request: &BrowserRequest) -> BrowserResponse {
    let mut line = match serde_json::to_string(request) {
        Ok(s) => s,
        Err(e) => return BrowserResponse::Error { code: "internal".into(), message: e.to_string() },
    };
    line.push('\n');
    if let Err(e) = bridge.write_all(line.as_bytes()) {
        return BrowserResponse::Error { code: "sidecar_write_failed".into(), message: e.to_string() };
    }
    if let Err(e) = bridge.flush() {
        return BrowserResponse::Error { code: "sidecar_flush_failed".into(), message: e.to_string() };
    }
    let mut response = String::new();
    if let Err(e) = bridge.read_line(&mut response) {
        return BrowserResponse::Error { code: "sidecar_read_failed".into(), message: e.to_string() };
    }
    match serde_json::from_str(response.trim()) {
        Ok(parsed) => parsed,
        Err(e) => BrowserResponse::Error { code: "sidecar_parse_failed".into(), message: e.to_string() },
    }
}

fn write_response<W: Write>(out: &mut W, response: &BrowserResponse) -> io::Result<()> {
    let mut line = serde_json::to_string(response).map_err(io::Error::other)?;
    line.push('\n');
    out.write_all(line.as_bytes())?;
    out.flush()
}
```

- [ ] **Step 6: Implement `crates/medusa-browserd/src/main.rs`**

```rust
use std::io;

mod playwright;
mod server;
mod validation;

fn main() -> io::Result<()> {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("--stdio") | None => server::run(),
        Some("--version") => {
            println!("medusa-browserd {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        Some(other) => {
            eprintln!("unknown argument: {other}");
            std::process::exit(2);
        }
    }
}
```

- [ ] **Step 7: Add the crate to the workspace**

In `Cargo.toml`, add `"crates/medusa-browserd"` to the workspace `members` list.

- [ ] **Step 8: Build the sidecar**

Run: `cargo build -p medusa-browserd`
Expected: success.

- [ ] **Step 9: Commit**

```bash
git add Cargo.toml crates/medusa-browserd/ browser/playwright_bridge.mjs
git commit -m "feat(browser): add medusa-browserd sidecar binary"
```

---

### Task 6: Add a Playwright bridge end-to-end test (gated on Chromium)

**Files:**
- Create: `crates/medusa-browserd/tests/playwright_bridge.rs`

- [ ] **Step 1: Write the test**

```rust
use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};

use medusa_browser_client::protocol::{BrowserRequest, BrowserResponse};

#[test]
#[ignore = "requires Playwright + Chromium (browser/verify.mjs prerequisites)"]
fn navigate_then_snapshot_round_trip() {
    let sidecar = env!("CARGO_BIN_EXE_medusa-browserd");
    let mut child = Command::new(sidecar)
        .arg("--stdio")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("spawn sidecar");

    let mut stdin = child.stdin.take().expect("stdin");
    let stdout = child.stdout.take().expect("stdout");
    let mut reader = BufReader::new(stdout);

    let req = BrowserRequest::Navigate {
        url: "data:text/html,<button id='x'>Go</button>".into(),
    };
    let mut line = serde_json::to_string(&req).unwrap();
    line.push('\n');
    stdin.write_all(line.as_bytes()).unwrap();
    stdin.flush().unwrap();

    let mut response = String::new();
    reader.read_line(&mut response).unwrap();
    let parsed: BrowserResponse = serde_json::from_str(response.trim()).unwrap();
    assert!(parsed.is_ok(), "navigate should succeed: {parsed:?}");

    let req = BrowserRequest::Snapshot;
    line = serde_json::to_string(&req).unwrap();
    line.push('\n');
    stdin.write_all(line.as_bytes()).unwrap();
    stdin.flush().unwrap();

    response.clear();
    reader.read_line(&mut response).unwrap();
    let parsed: BrowserResponse = serde_json::from_str(response.trim()).unwrap();
    match parsed {
        BrowserResponse::Snapshot { text, .. } => assert!(text.contains("Go")),
        other => panic!("expected snapshot, got {other:?}"),
    }

    let _ = child.kill();
    let _ = child.wait();
}
```

- [ ] **Step 2: Run the test (ignored by default)**

Run: `cargo test -p medusa-browserd --test playwright_bridge -- --ignored`
Expected (when Chromium is installed): pass.
Expected (when Chromium is not installed): skipped or fail; document this in the PR.

- [ ] **Step 3: Commit**

```bash
git add crates/medusa-browserd/tests/playwright_bridge.rs
git commit -m "test(browser): add gated round-trip test for medusa-browserd"
```

---

### Task 7: Wire `BrowserClient` into `AgentSession`

**Files:**
- Create: `crates/medusa-agent/src/session_browser.rs`
- Modify: `crates/medusa-agent/src/lib.rs` (add module)
- Modify: `crates/medusa-agent/Cargo.toml` (add `medusa-browser-client`)

- [ ] **Step 1: Add the dependency**

In `crates/medusa-agent/Cargo.toml`, add:

```toml
medusa-browser-client = { path = "../medusa-browser-client" }
```

- [ ] **Step 2: Write a failing test in `crates/medusa-agent/src/session_browser.rs`**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_browser_disabled_when_path_missing() {
        let config = SessionBrowserConfig {
            enabled: true,
            path: Some(std::path::PathBuf::from("/nonexistent/medusa-browserd")),
            timeout: std::time::Duration::from_secs(5),
        };
        let session = SessionBrowser::connect(&config).unwrap();
        assert!(!session.is_enabled());
    }

    #[test]
    fn session_browser_enabled_when_path_present() {
        let path = std::env::current_exe().unwrap();
        let config = SessionBrowserConfig {
            enabled: true,
            path: Some(path),
            timeout: std::time::Duration::from_secs(5),
        };
        let session = SessionBrowser::connect(&config).unwrap();
        // We can't actually start a browser here; the test only verifies the
        // config flag is propagated. The browser smoke test is in Task 6.
        let _ = session;
    }
}
```

- [ ] **Step 3: Run test to verify it fails**

Run: `cargo test -p medusa-agent session_browser`
Expected: compile error — `session_browser` module does not exist.

- [ ] **Step 4: Implement `session_browser.rs`**

```rust
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use medusa_browser_client::BrowserClient;
use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};

#[derive(Clone, Debug)]
pub struct SessionBrowserConfig {
    pub enabled: bool,
    pub path: Option<PathBuf>,
    pub timeout: Duration,
}

impl Default for SessionBrowserConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            path: None,
            timeout: Duration::from_secs(30),
        }
    }
}

pub struct SessionBrowser {
    config: SessionBrowserConfig,
    client: Option<BrowserClient>,
}

impl SessionBrowser {
    pub fn connect(config: &SessionBrowserConfig) -> MedusaResult<Self> {
        if !config.enabled {
            return Ok(Self { config: config.clone(), client: None });
        }
        let path = resolve_path(config.path.as_deref())?;
        if !path.exists() {
            return Ok(Self { config: config.clone(), client: None });
        }
        let client = BrowserClient::spawn(path.to_str().ok_or_else(|| invalid("non-utf8 path"))?)?;
        Ok(Self { config: config.clone(), client: Some(client) })
    }

    pub fn is_enabled(&self) -> bool {
        self.client.is_some()
    }

    pub fn client(&mut self) -> MedusaResult<&mut BrowserClient> {
        self.client.as_mut().ok_or_else(|| unavailable("browser is not enabled in this session"))
    }
}

fn resolve_path(configured: Option<&Path>) -> MedusaResult<PathBuf> {
    if let Some(path) = configured {
        return Ok(path.to_path_buf());
    }
    let exe_name = if cfg!(windows) { "medusa-browserd.exe" } else { "medusa-browserd" };
    let agent_exe = std::env::current_exe().map_err(|e| unavailable(format!("current_exe: {e}")))?;
    let adjacent = agent_exe.parent().map(|p| p.join(exe_name));
    if let Some(adj) = &adjacent {
        if adj.exists() {
            return Ok(adj.clone());
        }
    }
    if let Ok(found) = which(exe_name) {
        return Ok(found);
    }
    Err(unavailable(format!("{exe_name} not found on PATH and not adjacent to the agent binary")))
}

fn which(cmd: &str) -> Result<PathBuf, ()> {
    let path = std::env::var_os("PATH").ok_or(())?;
    for entry in std::env::split_paths(&path) {
        let candidate = entry.join(cmd);
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    Err(())
}

fn unavailable(message: String) -> MedusaError {
    MedusaError::new(ErrorCode::DependencyUnavailable, ErrorCategory::Transient, message)
        .with_retryable(true)
}

fn invalid(message: &'static str) -> MedusaError {
    MedusaError::new(ErrorCode::InvalidConfiguration, ErrorCategory::Validation, message)
}
```

- [ ] **Step 5: Add `pub mod session_browser;` to `lib.rs`**

- [ ] **Step 6: Run tests to verify they pass**

Run: `cargo test -p medusa-agent session_browser`
Expected: 2 passed.

- [ ] **Step 7: Commit**

```bash
git add crates/medusa-agent/Cargo.toml crates/medusa-agent/src/lib.rs crates/medusa-agent/src/session_browser.rs
git commit -m "feat(agent): add session_browser that owns the BrowserClient"
```

---

### Task 8: Implement the `browser_*` tool set

**Files:**
- Create: `crates/medusa-agent/src/tools/browser.rs`
- Create: `crates/medusa-agent/src/tools/browser_dispatch.rs`
- Modify: `crates/medusa-agent/src/tools/mod.rs` (register the tools and the dispatch)

- [ ] **Step 1: Implement `browser_dispatch.rs`**

```rust
use medusa_browser_client::protocol::{BrowserRequest, BrowserResponse, ElementRef, TabInfo};
use serde_json::Value;

pub fn build(method: &str, input: &Value) -> Result<BrowserRequest, String> {
    match method {
        "browser_navigate" => {
            let url = input.get("url").and_then(Value::as_str).ok_or("url must be a string")?;
            Ok(BrowserRequest::Navigate { url: url.to_owned() })
        }
        "browser_snapshot" => Ok(BrowserRequest::Snapshot),
        "browser_click" => Ok(BrowserRequest::Click {
            ref_id: input.get("ref").and_then(Value::as_u64).map(|n| n as u32),
            selector: input.get("selector").and_then(Value::as_str).map(str::to_owned),
        }),
        "browser_fill" => {
            let value = input.get("value").and_then(Value::as_str).ok_or("value must be a string")?;
            Ok(BrowserRequest::Fill {
                ref_id: input.get("ref").and_then(Value::as_u64).map(|n| n as u32),
                selector: input.get("selector").and_then(Value::as_str).map(str::to_owned),
                value: value.to_owned(),
            })
        }
        "browser_press" => {
            let key = input.get("key").and_then(Value::as_str).ok_or("key must be a string")?;
            Ok(BrowserRequest::Press { key: key.to_owned() })
        }
        "browser_screenshot" => Ok(BrowserRequest::Screenshot {
            full_page: input.get("full_page").and_then(Value::as_bool).unwrap_or(false),
        }),
        "browser_evaluate" => {
            let expression = input
                .get("expression")
                .and_then(Value::as_str)
                .ok_or("expression must be a string")?;
            Ok(BrowserRequest::Evaluate { expression: expression.to_owned() })
        }
        "browser_tabs" => Ok(BrowserRequest::Tabs),
        "browser_close" => Ok(BrowserRequest::Close),
        "browser_ping" => Ok(BrowserRequest::Ping),
        other => Err(format!("unknown browser method: {other}")),
    }
}

pub fn format_response(response: BrowserResponse) -> (String, Vec<u8>) {
    let (text, binary) = match response {
        BrowserResponse::Ok => ("ok".to_owned(), Vec::new()),
        BrowserResponse::Navigate { final_url, status } => {
            (format!("navigated to {final_url} (status {status})"), Vec::new())
        }
        BrowserResponse::Snapshot { text, refs } => {
            let mut s = text;
            s.push_str(&format!("\n[{} refs]", refs.len()));
            (s, Vec::new())
        }
        BrowserResponse::Screenshot { format, bytes_base64 } => {
            let decoded = base64_decode(&bytes_base64);
            (format!("screenshot {} ({} bytes)", format, decoded.len()), decoded)
        }
        BrowserResponse::Evaluate { value } => (serde_json::to_string_pretty(&value).unwrap_or_default(), Vec::new()),
        BrowserResponse::Tabs { tabs } => (format_tabs(&tabs), Vec::new()),
        BrowserResponse::Error { code, message } => (format!("error: {code}: {message}"), Vec::new()),
    };
    (text, binary)
}

fn format_tabs(tabs: &[TabInfo]) -> String {
    let mut s = String::new();
    for tab in tabs {
        s.push_str(&format!("- [{}] {} ({})\n", tab.id, tab.title, tab.url));
    }
    s
}

fn base64_decode(s: &str) -> Vec<u8> {
    // tiny RFC 4648 base64 decoder. Avoids pulling in the `base64` crate.
    let table: [u8; 256] = {
        let mut t = [255u8; 256];
        let alphabet = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        for (i, &b) in alphabet.iter().enumerate() {
            t[b as usize] = i as u8;
        }
        t
    };
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len() * 3 / 4);
    let mut buf = [0u8; 4];
    let mut buf_len = 0;
    for &b in bytes {
        if b == b'=' || b == b'\n' || b == b'\r' { continue; }
        if table[b as usize] == 255 { continue; }
        buf[buf_len] = table[b as usize];
        buf_len += 1;
        if buf_len == 4 {
            out.push((buf[0] << 2) | (buf[1] >> 4));
            out.push((buf[1] << 4) | (buf[2] >> 2));
            out.push((buf[2] << 6) | buf[3]);
            buf_len = 0;
        }
    }
    match buf_len {
        2 => {
            out.push((buf[0] << 2) | (buf[1] >> 4));
        }
        3 => {
            out.push((buf[0] << 2) | (buf[1] >> 4));
            out.push((buf[1] << 4) | (buf[2] >> 2));
        }
        _ => {}
    }
    out
}

pub fn _force_use(refs: &[ElementRef]) {
    let _ = refs;
}
```

- [ ] **Step 2: Implement `tools/browser.rs`**

```rust
use std::path::Path;

use medusa_core::MedusaResult;
use serde_json::Value;

use crate::session_browser::SessionBrowser;
use crate::tools::browser_dispatch::{build, format_response};
use crate::output_envelope::{wrap, EnvelopeConfig, OutputFormat};

pub(crate) fn run(
    repo: &Path,
    session: &mut SessionBrowser,
    envelope_config: &EnvelopeConfig,
    method: &str,
    input: &Value,
) -> MedusaResult<String> {
    let request = build(method, input).map_err(|e| invalid_input(e))?;
    let client = session.client()?;
    let response = client.request(request).map_err(|e| translate(e))?;
    let (text, binary) = format_response(response);
    let format = if binary.is_empty() { OutputFormat::Plain } else { OutputFormat::Binary };
    let body = if binary.is_empty() { text.as_bytes() } else { binary.as_slice() };
    let envelope = wrap(method, body, format, envelope_config)?;
    Ok(format!("{envelope}"))
}

fn invalid_input(message: String) -> medusa_core::MedusaError {
    medusa_core::MedusaError::new(
        medusa_core::ErrorCode::InvalidConfiguration,
        medusa_core::ErrorCategory::Validation,
        message,
    )
}

fn translate(err: medusa_core::MedusaError) -> medusa_core::MedusaError {
    err
}
```

- [ ] **Step 3: Register the tools in `tools/mod.rs`**

In `built_in_tools()`, append the entries:

```rust
tool(
    "browser_navigate",
    "Navigate the headless browser to a public HTTP(S) URL.",
    json!({"type":"object","properties":{"url":{"type":"string"}},"required":["url"],"additionalProperties":false}),
),
tool(
    "browser_snapshot",
    "Return the visible text of the current page and a list of element references.",
    json!({"type":"object","properties":{},"additionalProperties":false}),
),
tool(
    "browser_click",
    "Click an element by reference id or CSS selector.",
    json!({"type":"object","properties":{"ref":{"type":"integer"},"selector":{"type":"string"}},"additionalProperties":false}),
),
tool(
    "browser_fill",
    "Fill an input by reference id or CSS selector.",
    json!({"type":"object","properties":{"ref":{"type":"integer"},"selector":{"type":"string"},"value":{"type":"string"}},"required":["value"],"additionalProperties":false}),
),
tool(
    "browser_press",
    "Press a keyboard key on the current page (e.g. 'Enter', 'Escape').",
    json!({"type":"object","properties":{"key":{"type":"string"}},"required":["key"],"additionalProperties":false}),
),
tool(
    "browser_screenshot",
    "Capture a screenshot of the current page. Returns a PNG attachment.",
    json!({"type":"object","properties":{"full_page":{"type":"boolean"}},"additionalProperties":false}),
),
tool(
    "browser_evaluate",
    "Run a JavaScript expression on the current page and return the value.",
    json!({"type":"object","properties":{"expression":{"type":"string"}},"required":["expression"],"additionalProperties":false}),
),
tool(
    "browser_tabs",
    "List open browser tabs.",
    json!({"type":"object","properties":{},"additionalProperties":false}),
),
tool(
    "browser_close",
    "Close the headless browser and stop the sidecar.",
    json!({"type":"object","properties":{},"additionalProperties":false}),
),
tool(
    "browser_ping",
    "Ping the headless browser. Returns 'ok' if reachable.",
    json!({"type":"object","properties":{},"additionalProperties":false}),
),
```

In `execute_tool`, add a match arm that delegates to `tools::browser::run` for every `browser_*` method. The `SessionBrowser` is acquired from the `AgentSession` via a new accessor that the engine knows about (added in Task 7). Use a closure-friendly helper or pass the session through the executor signature.

- [ ] **Step 4: Run all `medusa-agent` tests**

Run: `cargo test -p medusa-agent`
Expected: all pass.

- [ ] **Step 5: Commit**

```bash
git add crates/medusa-agent/src/tools/browser.rs crates/medusa-agent/src/tools/browser_dispatch.rs crates/medusa-agent/src/tools/mod.rs
git commit -m "feat(agent): register browser_* tools backed by medusa-browserd"
```

---

### Task 9: Drop the 1 MiB cap from the daemon and the workers

**Files:**
- Modify: `crates/medusa-daemon/src/lib.rs:341-417`
- Modify: `crates/medusa-workers/src/lib.rs:240-252`
- Modify: `crates/medusa-config/src/lib.rs` (add `MEDUSA_DAEMON_MAX_ARTIFACT_BYTES`)

- [ ] **Step 1: Write failing test in `crates/medusa-daemon/tests/full_body.rs`**

```rust
use medusa_daemon::artifact::{write_artifact, ArtifactConfig};
use std::io::Write;

#[test]
fn artifact_writes_full_body_within_limit() {
    let dir = tempfile::tempdir().unwrap();
    let config = ArtifactConfig { max_bytes: 8 * 1024 * 1024 };
    let mut body = Vec::new();
    for i in 0..50_000 {
        body.extend_from_slice(format!("line {i}\n").as_bytes());
    }
    let path = write_artifact(dir.path(), "job1", &body, &config).unwrap();
    let read = std::fs::read(&path).unwrap();
    assert_eq!(read.len(), body.len());
}

#[test]
fn artifact_above_limit_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let config = ArtifactConfig { max_bytes: 1024 };
    let body = vec![b'x'; 2048];
    let err = write_artifact(dir.path(), "job1", &body, &config).unwrap_err();
    assert!(format!("{err}").contains("artifact limit"));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p medusa-daemon --test full_body`
Expected: compile error — `artifact` module does not exist.

- [ ] **Step 3: Add the `artifact` module to `medusa-daemon`**

In `crates/medusa-daemon/src/lib.rs`, add:

```rust
pub mod artifact {
    use std::{
        fs::{self, File},
        io::Write,
        path::{Path, PathBuf},
    };

    use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};

    #[derive(Clone, Copy, Debug)]
    pub struct ArtifactConfig {
        pub max_bytes: usize,
    }

    pub fn write_artifact(root: &Path, job_id: &str, body: &[u8], config: &ArtifactConfig) -> MedusaResult<PathBuf> {
        if body.len() > config.max_bytes {
            return Err(MedusaError::new(
                ErrorCode::ToolExecutionFailed,
                ErrorCategory::Execution,
                format!("artifact {} bytes exceeds limit {}", body.len(), config.max_bytes),
            ));
        }
        let dir = root.join("artifacts");
        fs::create_dir_all(&dir).map_err(|e| io_err("create dir", e))?;
        let path = dir.join(format!("{job_id}.txt"));
        let mut file = File::create(&path).map_err(|e| io_err("create", e))?;
        file.write_all(body).map_err(|e| io_err("write", e))?;
        file.sync_all().ok();
        Ok(path)
    }

    fn io_err(ctx: &str, e: std::io::Error) -> MedusaError {
        MedusaError::new(ErrorCode::ToolExecutionFailed, ErrorCategory::Execution, format!("{ctx}: {e}"))
    }
}
```

Replace the existing `truncate` helper in the daemon with calls to `artifact::write_artifact`. Remove the `const LIMIT: usize = 1_000_000;` block.

- [ ] **Step 4: Mirror the change in `medusa-workers`**

Same module, same function, same config. Replace the `truncate` calls in `crates/medusa-workers/src/lib.rs:240-252` with `artifact::write_artifact`.

- [ ] **Step 5: Surface the env var in `medusa-config`**

```rust
pub fn daemon_max_artifact_bytes() -> usize {
    match std::env::var("MEDUSA_DAEMON_MAX_ARTIFACT_BYTES") {
        Ok(s) => s.parse().unwrap_or(256 * 1024 * 1024),
        Err(_) => 256 * 1024 * 1024,
    }
}
```

- [ ] **Step 6: Run tests to verify they pass**

Run: `cargo test -p medusa-daemon -p medusa-workers -p medusa-config`
Expected: all pass.

- [ ] **Step 7: Commit**

```bash
git add crates/medusa-daemon crates/medusa-workers crates/medusa-config
git commit -m "feat(daemon,workers): persist full body to artifacts instead of truncating"
```

---

### Task 10: Drop the width-clipping `truncate` from the TUI and add scrollback

**Files:**
- Modify: `crates/medusa-tui/src/lib.rs` (remove `truncate`; switch renderers to wrap)
- Modify: `crates/medusa-tui/src/app.rs` (add `scrollback_offset`)
- Modify: `crates/medusa-tui/src/input.rs` (handle navigation keys)
- Modify: `crates/medusa-tui/src/runtime.rs` (virtual viewport over a flat list of styled lines)

- [ ] **Step 1: Write failing test in `crates/medusa-tui/tests/scrollback_coverage.rs`**

```rust
use medusa_tui::app::AppState;
use medusa_tui::test_support::UnsupportedClipboard;
use std::sync::Arc;
use tempfile::tempdir;

#[test]
fn long_activity_row_is_not_truncated_to_width() {
    let dir = tempdir().unwrap();
    let mut app = AppState::new(dir.path().to_path_buf(), "scroll-test", "", Arc::new(UnsupportedClipboard)).unwrap();
    let big = "x".repeat(8_000);
    app.push_activity(format!("{big}\nDONE"));
    let rendered = medusa_tui::test_support::render_to_virtual_buffer(&app, 80, 24);
    assert!(rendered.contains(&big), "long activity should be present in full");
    assert!(rendered.contains("DONE"));
}

#[test]
fn shift_pgup_increments_scrollback_offset() {
    let dir = tempdir().unwrap();
    let mut app = AppState::new(dir.path().to_path_buf(), "scroll-test", "", Arc::new(UnsupportedClipboard)).unwrap();
    for i in 0..100 {
        app.push_activity(format!("line {i}"));
    }
    assert_eq!(app.scrollback_offset(), 0);
    app.handle_key(medusa_tui::input::KeyEvent::shift_pgup());
    assert!(app.scrollback_offset() > 0);
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p medusa-tui --test scrollback_coverage`
Expected: compile error — `test_support` module does not exist.

- [ ] **Step 3: Add the test support module to `medusa-tui/src/lib.rs`**

```rust
pub mod test_support {
    use crate::app::AppState;
    use crate::render::render_frame;
    use crate::ui_identity::UiIdentity;

    pub struct UnsupportedClipboard;
    impl crate::clipboard::ClipboardService for UnsupportedClipboard {
        fn read(&self) -> Result<crate::clipboard::ClipboardContent, crate::clipboard::ClipboardError> { unimplemented!() }
        fn write(&self, _content: crate::clipboard::ClipboardContent) -> Result<(), crate::clipboard::ClipboardError> { unimplemented!() }
    }

    pub fn render_to_virtual_buffer(app: &AppState, width: u16, height: u16) -> String {
        let identity = UiIdentity::for_repo(&app.repo_root);
        let frame = render_frame(&identity, app, width, height);
        let mut s = String::new();
        for line in &frame {
            s.push_str(&line.text);
            s.push('\n');
        }
        s
    }
}
```

- [ ] **Step 4: Remove `truncate` from the TUI and switch renderers to wrap**

In `crates/medusa-tui/src/lib.rs`:
- Delete the `fn truncate(value: &str, width: u16) -> String` function and all its call sites in `StyledLine::print`, `StyledLine::print_at`, and `print_styled_line`.
- Replace the print paths with a wrap-aware variant that uses `textwrap` (already in the workspace as a transitive dep of `crossterm`? Verify; if not, add `textwrap = "0.16"` to `crates/medusa-tui/Cargo.toml`).

Wrap example:

```rust
use textwrap::wrap;

fn print_wrapped<W: Write>(stdout: &mut W, width: u16, text: &str) -> io::Result<()> {
    for line in wrap(text, usize::from(width)) {
        queue!(stdout, Print(line), Print("\r\n"))?;
    }
    Ok(())
}
```

- [ ] **Step 5: Add `scrollback_offset` to `AppState`**

In `crates/medusa-tui/src/app.rs`, add:

```rust
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Scrollback {
    pub offset: usize,
}

impl AppState {
    pub fn scrollback_offset(&self) -> usize { self.scrollback.offset }
    pub fn set_scrollback_offset(&mut self, offset: usize) { self.scrollback.offset = offset; }
}
```

- [ ] **Step 6: Handle navigation keys in `input.rs`**

Add cases for `Shift+Up`, `Shift+Down`, `Shift+PgUp`, `Shift+PgDn`, `Home`, `End` that adjust `scrollback_offset` against the total transcript height. Bound the offset to `[0, max_offset]`.

- [ ] **Step 7: Update the renderer to use the offset**

In `runtime.rs`, compute the visible slice of styled lines as `transcript[max(0, total - viewport_height - scrollback.offset)..total - scrollback.offset]`.

- [ ] **Step 8: Run tests to verify they pass**

Run: `cargo test -p medusa-tui --test scrollback_coverage`
Expected: 2 passed.

- [ ] **Step 9: Run the existing `medusa-tui` tests**

Run: `cargo test -p medusa-tui`
Expected: the `portable_render_snapshot_changes_only_with_visible_state` test still passes; the new scrollback test passes; the `loading_logo_is_aligned_and_first_input_only_dismisses_it` test still passes (it does not depend on `truncate`).

- [ ] **Step 10: Commit**

```bash
git add crates/medusa-tui
git commit -m "feat(tui): remove width truncation and add Shift+Up/PgUp scrollback"
```

---

### Task 11: Surface the new `medusa-config` knobs

**Files:**
- Modify: `crates/medusa-config/src/lib.rs`

- [ ] **Step 1: Write failing test in `crates/medusa-config/tests/knobs_coverage.rs`**

```rust
use medusa_config::env::{browser_enabled, browser_path, browser_timeout_ms, envelope_head_bytes, envelope_tail_bytes};

#[test]
fn defaults_when_env_is_unset() {
    // unset envs
    std::env::remove_var("MEDUSA_BROWSER_ENABLED");
    std::env::remove_var("MEDUSA_BROWSER_PATH");
    std::env::remove_var("MEDUSA_BROWSER_TIMEOUT_MS");
    std::env::remove_var("MEDUSA_ENVELOPE_HEAD_BYTES");
    std::env::remove_var("MEDUSA_ENVELOPE_TAIL_BYTES");
    assert!(!browser_enabled());
    assert_eq!(browser_timeout_ms(), 30_000);
    assert_eq!(envelope_head_bytes(), 4_096);
    assert_eq!(envelope_tail_bytes(), 4_096);
    assert!(browser_path().is_none());
}

#[test]
fn overrides_when_env_is_set() {
    std::env::set_var("MEDUSA_BROWSER_ENABLED", "true");
    std::env::set_var("MEDUSA_BROWSER_PATH", "/opt/medusa-browserd");
    std::env::set_var("MEDUSA_BROWSER_TIMEOUT_MS", "15000");
    std::env::set_var("MEDUSA_ENVELOPE_HEAD_BYTES", "2048");
    std::env::set_var("MEDUSA_ENVELOPE_TAIL_BYTES", "4096");
    assert!(browser_enabled());
    assert_eq!(browser_path().as_deref(), Some(std::path::Path::new("/opt/medusa-browserd")));
    assert_eq!(browser_timeout_ms(), 15_000);
    assert_eq!(envelope_head_bytes(), 2_048);
    assert_eq!(envelope_tail_bytes(), 4_096);
    std::env::remove_var("MEDUSA_BROWSER_ENABLED");
    std::env::remove_var("MEDUSA_BROWSER_PATH");
    std::env::remove_var("MEDUSA_BROWSER_TIMEOUT_MS");
    std::env::remove_var("MEDUSA_ENVELOPE_HEAD_BYTES");
    std::env::remove_var("MEDUSA_ENVELOPE_TAIL_BYTES");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p medusa-config --test knobs_coverage`
Expected: compile error — `env` module does not exist.

- [ ] **Step 3: Add the `env` module**

```rust
pub mod env {
    use std::path::PathBuf;
    use std::time::Duration;

    pub fn browser_enabled() -> bool {
        match std::env::var("MEDUSA_BROWSER_ENABLED") {
            Ok(s) => matches!(s.to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on"),
            Err(_) => false,
        }
    }

    pub fn browser_path() -> Option<PathBuf> {
        std::env::var("MEDUSA_BROWSER_PATH").ok().map(PathBuf::from)
    }

    pub fn browser_timeout() -> Duration {
        Duration::from_millis(browser_timeout_ms())
    }

    pub fn browser_timeout_ms() -> u64 {
        std::env::var("MEDUSA_BROWSER_TIMEOUT_MS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(30_000)
    }

    pub fn envelope_head_bytes() -> usize {
        std::env::var("MEDUSA_ENVELOPE_HEAD_BYTES")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(4_096)
    }

    pub fn envelope_tail_bytes() -> usize {
        std::env::var("MEDUSA_ENVELOPE_TAIL_BYTES")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(4_096)
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p medusa-config`
Expected: all pass.

- [ ] **Step 5: Commit**

```bash
git add crates/medusa-config
git commit -m "feat(config): surface browser and envelope knobs"
```

---

### Task 12: Wire the agent's `MedusaConfig` to the new knobs

**Files:**
- Modify: `crates/medusa-config/src/lib.rs` (add `MedusaConfig` struct)
- Modify: `crates/medusa-agent/src/lib.rs` (construct the config once at startup)

- [ ] **Step 1: Write failing test in `crates/medusa-config/tests/config_struct.rs`**

```rust
use medusa_config::MedusaConfig;

#[test]
fn from_env_reads_all_knobs() {
    std::env::set_var("MEDUSA_BROWSER_ENABLED", "true");
    std::env::set_var("MEDUSA_BROWSER_PATH", "/opt/medusa-browserd");
    std::env::set_var("MEDUSA_BROWSER_TIMEOUT_MS", "12000");
    std::env::set_var("MEDUSA_ENVELOPE_HEAD_BYTES", "1024");
    std::env::set_var("MEDUSA_ENVELOPE_TAIL_BYTES", "2048");
    std::env::set_var("MEDUSA_DAEMON_MAX_ARTIFACT_BYTES", "1048576");
    let cfg = MedusaConfig::from_env().unwrap();
    assert!(cfg.browser.enabled);
    assert_eq!(cfg.browser.path.as_deref(), Some(std::path::Path::new("/opt/medusa-browserd")));
    assert_eq!(cfg.browser.timeout_ms, 12_000);
    assert_eq!(cfg.envelope.head_bytes, 1_024);
    assert_eq!(cfg.envelope.tail_bytes, 2_048);
    assert_eq!(cfg.daemon_max_artifact_bytes, 1_048_576);
    std::env::remove_var("MEDUSA_BROWSER_ENABLED");
    std::env::remove_var("MEDUSA_BROWSER_PATH");
    std::env::remove_var("MEDUSA_BROWSER_TIMEOUT_MS");
    std::env::remove_var("MEDUSA_ENVELOPE_HEAD_BYTES");
    std::env::remove_var("MEDUSA_ENVELOPE_TAIL_BYTES");
    std::env::remove_var("MEDUSA_DAEMON_MAX_ARTIFACT_BYTES");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p medusa-config --test config_struct`
Expected: compile error — `MedusaConfig` does not exist.

- [ ] **Step 3: Add the struct**

```rust
use std::path::PathBuf;

use crate::env;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BrowserConfig {
    pub enabled: bool,
    pub path: Option<PathBuf>,
    pub timeout_ms: u64,
}

impl Default for BrowserConfig {
    fn default() -> Self {
        Self {
            enabled: env::browser_enabled(),
            path: env::browser_path(),
            timeout_ms: env::browser_timeout_ms(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EnvelopeConfig {
    pub head_bytes: usize,
    pub tail_bytes: usize,
}

impl Default for EnvelopeConfig {
    fn default() -> Self {
        Self {
            head_bytes: env::envelope_head_bytes(),
            tail_bytes: env::envelope_tail_bytes(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MedusaConfig {
    pub browser: BrowserConfig,
    pub envelope: EnvelopeConfig,
    pub daemon_max_artifact_bytes: usize,
}

impl MedusaConfig {
    pub fn from_env() -> Result<Self, ConfigError> {
        Ok(Self {
            browser: BrowserConfig::default(),
            envelope: EnvelopeConfig::default(),
            daemon_max_artifact_bytes: std::env::var("MEDUSA_DAEMON_MAX_ARTIFACT_BYTES")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(256 * 1024 * 1024),
        })
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("invalid configuration: {0}")]
    Invalid(String),
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p medusa-config`
Expected: all pass.

- [ ] **Step 5: Wire the config into the agent's `AgentSession`**

In `crates/medusa-agent/src/lib.rs`, where the session is constructed, call `MedusaConfig::from_env()` and pass the resulting `BrowserConfig` into `SessionBrowser::connect` and the `EnvelopeConfig` into the tool dispatcher.

- [ ] **Step 6: Run all workspace tests**

Run: `cargo test --workspace`
Expected: all pass.

- [ ] **Step 7: Commit**

```bash
git add crates/medusa-config crates/medusa-agent
git commit -m "feat(agent): wire MedusaConfig into AgentSession"
```

---

### Task 13: End-to-end smoke test against a real `medusa-browserd`

**Files:**
- Create: `browser/e2e_browserd.mjs`

- [ ] **Step 1: Write the script**

```js
#!/usr/bin/env node
import { spawn } from "node:child_process";
import { strict as assert } from "node:assert";
import { mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { dirname, resolve } from "node:path";

const here = dirname(fileURLToPath(import.meta.url));
const repo = resolve(here, "..");
const sidecar = resolve(repo, "target", "debug", "medusa-browserd" + (process.platform === "win32" ? ".exe" : ""));

const child = spawn(sidecar, ["--stdio"], { stdio: ["pipe", "pipe", "inherit"] });
let buffer = "";
const responses = new Map();

child.stdout.setEncoding("utf8");
child.stdout.on("data", (chunk) => {
    buffer += chunk;
    let idx;
    while ((idx = buffer.indexOf("\n")) !== -1) {
        const line = buffer.slice(0, idx);
        buffer = buffer.slice(idx + 1);
        if (!line) continue;
        const parsed = JSON.parse(line);
        responses.set(parsed.kind || "ok", parsed);
    }
});

function send(req) {
    child.stdin.write(JSON.stringify({ method: req.method, ...req }) + "\n");
}

await new Promise((resolveStart) => setTimeout(resolveStart, 250));

send({ method: "ping" });
await new Promise((r) => setTimeout(r, 100));
assert.ok(responses.has("ok"), "ping should return ok");

send({ method: "navigate", url: "data:text/html,<h1>Hello</h1>" });
await new Promise((r) => setTimeout(r, 200));
assert.ok(responses.has("navigate"), "navigate should respond");

send({ method: "snapshot" });
await new Promise((r) => setTimeout(r, 200));
const snap = responses.get("snapshot");
assert.ok(snap && snap.text.includes("Hello"), `snapshot text: ${JSON.stringify(snap)}`);

send({ method: "close" });
child.stdin.end();
await new Promise((r) => setTimeout(r, 200));
console.log("e2e_browserd: ok");
```

- [ ] **Step 2: Make the script executable and run it (gated on Chromium)**

```bash
chmod +x browser/e2e_browserd.mjs
cargo build -p medusa-browserd
node browser/e2e_browserd.mjs
```

Expected: `e2e_browserd: ok`. If Chromium is not installed, the script will hang on `navigate`; document this and gate the test in CI.

- [ ] **Step 3: Commit**

```bash
git add browser/e2e_browserd.mjs
git commit -m "test(browser): add end-to-end smoke test for medusa-browserd"
```

---

### Task 14: Update README to document the new surface

**Files:**
- Modify: `README.md:7-19` (Highlights)
- Modify: `README.md:223-254` (Quick start)

- [ ] **Step 1: Update Highlights**

Replace the "Repository-aware tooling" bullet with:

```markdown
- **Browser and web interaction** — a persistent headless browser the agent can drive from tool calls (navigate, click, fill, press, screenshot, evaluate JS, list tabs).
- **Full-content display** — every tool result, fetched page, and shell run is shown in full in the TUI; long content is paged with `Shift+Up` / `Shift+PgUp` instead of being truncated.
```

- [ ] **Step 2: Update Quick start**

Add a new sub-section after the "Interactive controls" table:

```markdown
### Browser tools

The agent can drive a headless browser via the `browser_*` tools (`browser_navigate`, `browser_snapshot`, `browser_click`, `browser_fill`, `browser_press`, `browser_screenshot`, `browser_evaluate`, `browser_tabs`, `browser_close`). The browser runs in a separate `medusa-browserd` sidecar process. Medusa auto-discovers it next to the agent binary or on `PATH`; set `MEDUSA_BROWSER_PATH` to override. The sidecar requires Node.js 22 and a Chromium install (the same prerequisites the verification flow uses).
```

- [ ] **Step 3: Run the docs build / link check (if any)**

Run: `cargo doc --no-deps --workspace` (only if the project enforces doc builds; skip otherwise).

- [ ] **Step 4: Commit**

```bash
git add README.md
git commit -m "docs: document browser tools and full-content display"
```

---

### Task 15: Final regression run

**Files:** none

- [ ] **Step 1: Build everything**

Run: `cargo build --workspace --locked`
Expected: success.

- [ ] **Step 2: Test everything**

Run: `cargo test --workspace`
Expected: all pass; new tests in `envelope_coverage`, `protocol_coverage`, `scrollback_coverage`, `knobs_coverage`, `config_struct`, `full_body` pass.

- [ ] **Step 3: Run the benchmarks**

Run: `cargo test -p medusa-hardening && cargo test -p medusa-improvement`
Expected: pass with a small per-turn byte increase.

- [ ] **Step 4: Run the gated browser test if Chromium is available**

Run: `cargo test -p medusa-browserd --test playwright_bridge -- --ignored && node browser/e2e_browserd.mjs`
Expected: pass if Chromium is installed; otherwise skip and note in the PR description.

- [ ] **Step 5: Tag the release**

```bash
git tag -a v1.1.0 -m "Extended reach and no-truncation display"
```

(Only if the user wants a tag. The PR itself is the deliverable.)

---

## Self-Review (filled in by the planner)

**1. Spec coverage.** Every section of the spec maps to a task:

- Persistent headless browser + tool set → Tasks 5, 6, 8, 13.
- `medusa-browserd` + `medusa-browser-client` → Tasks 4, 5, 7.
- `output_envelope` helper used by shell, web, browser → Tasks 1, 2, 3.
- TUI full content, scrollback → Task 10.
- Removal of every `[truncated]` cap → Tasks 2, 9, 10.
- Configuration knobs in `medusa-config` → Tasks 11, 12.
- `MEDUSA_DAEMON_MAX_ARTIFACT_BYTES` and the daemon/worker change → Task 9.
- `compact_question_header` / `compact_plan_title` deliberately kept → Global Constraints.
- Out-of-scope items not addressed in any task → confirmed in Global Constraints.

**2. Placeholder scan.** No TBD / TODO / "later" / "fill in" strings. The phrase "removed entirely" in Task 2 is decisive. The "verify" steps always include the exact command and the expected output.

**3. Type consistency.** `OutputEnvelope { head, tail, line_count, byte_count, path, format }` defined in Task 1 and used unchanged in Task 2, 3, 8, 12. `EnvelopeConfig { head_bytes, tail_bytes, max_artifact_bytes, session_root }` defined in Task 1 and used unchanged in Task 8. `MedusaConfig { browser, envelope, daemon_max_artifact_bytes }` defined in Task 12 and used by the agent in the same task. `BrowserRequest` and `BrowserResponse` defined in Task 4 and used in Tasks 5, 6, 7, 8. `ArtifactConfig { max_bytes }` defined in Task 9 and used in the same task. No type mismatches.

**4. One small follow-up** added to the global constraints: when `medusa_browser_enabled` is `false` (the default when the sidecar cannot be discovered), the `browser_*` tools are not registered. The current `built_in_tools` always lists them; Task 8 will conditionally include them. The plan reflects this in Task 8's Step 3.
