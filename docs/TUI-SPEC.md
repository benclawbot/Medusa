# Medusa Interactive TUI Specification

**Status:** Accepted for implementation  
**Scope:** Replace the current read-only daemon dashboard with the default interactive Medusa experience, launched by typing `medusa`.  
**Target:** Production-ready terminal coding agent with headless CLI compatibility.

## 1. Executive decision

The default command becomes:

```bash
medusa
```

This launches the interactive TUI in the current directory.

Existing automation-oriented commands remain available:

```bash
medusa run "objective"
medusa resume <session-id>
medusa doctor
medusa migrate
medusa search <pattern>
medusa shell <program> [args...]
medusa checkpoint "message"
```

The separate `medusa-tui` binary remains temporarily as a compatibility alias, prints a deprecation notice, and launches the same implementation.

## 2. Product goals

1. A new user can install Medusa, enter a repository, type `medusa`, describe a task, and see useful progress without reading documentation.
2. The main screen is a coding session, not a daemon monitor.
3. Autonomous execution remains understandable: plan, active operation, changes, verification, and final evidence are visible.
4. Recovering from interruption, failure, or a bad change is obvious.
5. Expert users retain headless commands and scriptable output.
6. Security boundaries remain explicit and non-bypassable.
7. Linux, macOS, Windows CMD, PowerShell, Windows Terminal, tmux, and WSL behave consistently.

## 3. Invocation contract

| Command | Required behavior |
|---|---|
| `medusa` | Launch interactive TUI in the current directory |
| `medusa --repo PATH` | Launch TUI for `PATH` |
| `medusa "fix the failing tests"` | Launch TUI and submit or prefill the initial objective |
| `medusa --continue` | Resume the most recent session for the repository |
| `medusa --resume SESSION` | Open a selected session in the TUI |
| `medusa run "objective"` | Preserve headless run-to-completion behavior |
| `medusa -p "objective"` | Script-friendly single-shot execution |
| piped stdin + `-p` | Include stdin as task context and run non-interactively |
| `medusa-tui` | Compatibility alias for interactive mode |

Rules:

- Known subcommands retain their current meanings.
- No subcommand routes to the TUI.
- Mistyped known subcommands produce suggestions instead of silently becoming prompts.
- Non-interactive stdin/stdout fails cleanly unless a headless mode is requested.
- `--help` clearly separates interactive and automation use.

## 4. Main interface

The default layout contains:

1. **Header** — repository, branch, dirty state, model/provider, autonomy mode, session ID, connection state.
2. **Transcript** — user prompts, assistant responses, tool calls, shell output, diffs, warnings, policy denials, retries, verification, workers.
3. **Task panel** — planned, running, completed, failed, blocked, and skipped steps; background jobs and workers.
4. **Composer** — multiline editing, file/symbol mentions, slash commands, shell mode, attachments where supported.
5. **Footer** — key hints, elapsed time, context use, background job count, approvals, checkpoint.

The transcript and composer remain usable at 80×24. Secondary panels collapse on narrow terminals.

## 5. Functional requirements

### 5.1 Conversational coding

- Natural-language objectives and follow-up prompts.
- Continue chatting while work is active.
- Interrupt and redirect without losing completed evidence.
- Stream assistant output, tool activity, and command output.
- Legible Markdown, code blocks, paths, errors, and evidence.
- Copy/export transcript and external-editor support.

### 5.2 Repository awareness

- Repository root, branch, worktree state, changed-file count.
- `@` completion for files, directories, symbols, sessions, and evidence.
- Path completion and quick file view.
- Bootstrap flow when `.medusa` is absent.
- Clear handling of missing, unsupported, and non-Git repositories.

### 5.3 Planning and progress

- Structured execution checklist.
- Real-time pending/running/completed/failed/blocked/skipped states.
- Plan review and editing in plan/manual modes.
- Compact and detailed views.
- Durable persisted plan state.

### 5.4 Tool execution

- Tool identity, arguments summary, timing, outcome, and evidence.
- Streaming shell output with retained final output.
- Successful low-value calls collapsed; failures expanded.
- Direct shell mode via `! command`, routed through policy and sandboxing.
- Eligible commands may be backgrounded.
- Stop, retry, inspect logs, attach, and foreground controls.
- Clear distinction between sandbox, browser, MCP, hook, and worker execution.

### 5.5 Diff and review

- Changed files and line counts.
- Inline or side-by-side diff according to terminal width.
- File and hunk navigation.
- Approve/reject all, file, or hunk where review is required.
- Guarded file/hunk revert.
- Formatter changes distinguished where possible.
- Checkpoint shown before high-risk transactions.
- Existing containment, rollback, and protected-verification controls remain mandatory.

### 5.6 Autonomy modes

Required modes:

- **Plan** — inspect and plan, no mutation.
- **Manual** — prompt for risky tools and mutations.
- **Accept edits** — edits proceed; sensitive commands still require approval.
- **Auto** — classifier permits low-risk actions and prompts for high-risk actions.
- **YOLO** — maximum permitted autonomy, still subject to hard denies and sandbox boundaries.

The mode is always visible, persisted, auditable, and changeable through `/mode` and a keyboard shortcut. Approval dialogs bind to the exact operation, command, files, diff, and risk class. Stale approvals are rejected.

### 5.7 Sessions and recovery

- New, continue latest, search, resume, rename, and archive.
- Objective, timestamps, status, branch, changed files, and last evidence.
- Recovery after terminal or daemon interruption.
- Accurate incomplete/failed state.
- Rewind conversation and guarded repository state to checkpoints.
- Safe attach from another terminal.

### 5.8 Jobs and workers

- Active/completed job and worker list.
- Owner session, task, state, duration, worktree/branch, and recent output.
- Attach, stop, stop all, retry, and logs.
- Deterministic conflict behavior and visible cleanup failures.
- Never delete another worker’s active worktree.

### 5.9 Verification and completion

- Tests, lint, formatting, docs, security, browser, and package checks shown separately.
- Streaming verification output.
- Conclusions linked to evidence.
- Final summary with objective status, files, checks, risks, checkpoint/commit, and session ID.
- “Complete” is impossible unless the engine’s completion and evidence contract passes.

### 5.10 Slash command menu

Typing `/` opens a searchable command menu. Minimum commands:

`/help`, `/new`, `/continue`, `/resume`, `/sessions`, `/clear`, `/compact`, `/plan`, `/tasks`, `/mode`, `/model`, `/diff`, `/files`, `/evidence`, `/checkpoint`, `/rewind`, `/memory`, `/skills`, `/hooks`, `/mcp`, `/browser`, `/workers`, `/jobs`, `/doctor`, `/config`, `/theme`, `/keys`, `/quit`.

Skills and MCP-contributed commands appear with provenance labels.

### 5.11 Input quality

- Multiline editing with configurable submit/newline bindings.
- History, reverse search, undo/redo, word movement and deletion.
- Optional Vim mode.
- `$EDITOR`/`$VISUAL` draft editing.
- Draft preservation across dialogs.
- Repository-aware suggestions with an off switch.
- Safe multiline paste handling.
- Image input only when the provider supports it; otherwise explain the limitation.

### 5.12 Onboarding and configuration

First launch covers repository detection, bootstrap, provider/model, credential status, sandbox, browser sidecar, default mode, and key tutorial.

`/doctor` provides actionable checks and safe fixes without exposing secrets.

### 5.13 Memory and extensions

- Inspect relevant memory and proposals.
- Approve, reject, supersede, or forget proposals.
- Loaded skills with provenance, checksum, and trust.
- Hooks and failure policy.
- MCP servers, state, tools, and trust failures.
- Browser steps, screenshots/evidence, assertions, and failures.
- Poisoning, checksum, credential, and provenance failures are visible and actionable.

### 5.14 Accessibility and compatibility

- Screen-reader/plain-output mode.
- No color-only meaning.
- Configurable theme and reduced motion.
- Full keyboard control.
- Reliable redraw and terminal restoration.
- Graceful fallback without alternate screen or advanced colors.
- Unicode and ASCII border modes.
- Linux, macOS, Windows CMD, PowerShell, Windows Terminal, tmux, and WSL coverage.

## 6. Keyboard baseline

| Shortcut | Behavior |
|---|---|
| `Enter` | Submit |
| `Shift+Enter` or configured alternative | New line |
| `Ctrl+C` | Interrupt; clear when idle; second idle press exits |
| `Ctrl+D` | Exit when composer is empty |
| `Esc` | Close dialog or interrupt |
| `Ctrl+L` | Full redraw |
| `Ctrl+R` | Reverse history search |
| `Ctrl+O` | Toggle transcript/tool details |
| `Ctrl+T` | Toggle task panel |
| `Ctrl+B` | Background eligible command |
| `Ctrl+G` | Edit draft externally |
| `Shift+Tab` | Cycle autonomy mode |
| `Alt+P` | Switch model |
| `?` | Contextual help |
| `/` | Command palette |
| `!` at start | Direct shell mode |
| `@` | File/symbol/context picker |

All bindings are configurable and discoverable through `/keys`.

## 7. Architecture

### 7.1 Reusable TUI library

Convert `medusa-tui` from binary-only into:

```text
crates/medusa-tui/src/
  lib.rs
  app.rs
  event.rs
  ui/
  input/
  session/
  approvals/
  diff/
  tasks/
  commands/
  terminal.rs
  main.rs
```

Expose:

```rust
pub fn run(options: TuiOptions) -> MedusaResult<ExitReason>;
```

Dependency direction:

```text
medusa-cli -> medusa-tui -> runtime/application interfaces
```

`medusa-tui` must not depend on `medusa-cli`.

### 7.2 Event-driven runtime

Replace polling-only dashboard behavior with a versioned sequenced event stream:

- session created/loaded/updated
- assistant delta
- tool started/output/completed
- plan changed
- approval requested/resolved
- file change/diff
- verification started/output/completed
- worker/job state
- memory proposal
- completion/failure

Support reconnect replay. Polling may remain only as fallback.

### 7.3 Daemon lifecycle

Bare `medusa` must automatically connect or safely start the repository daemon, wait for readiness, recover state, attach the TUI, and keep background work alive after disconnect. Expose `medusa daemon status|start|stop|logs`.

### 7.4 Cross-platform transport

Introduce a transport abstraction:

- Unix domain sockets on Linux/macOS.
- Windows named pipes on Windows.
- Authenticated loopback TCP only as an explicit fallback.

Use platform-appropriate ownership, locking, and process liveness.

### 7.5 Rendering and concurrency

Use Ratatui over Crossterm with reducer/state-machine architecture. No model, shell, filesystem, or daemon operation blocks the render thread. Use bounded channels, panic-safe terminal restoration, snapshot-testable views, and deterministic event replay.

## 8. Security requirements

- Every action goes through existing policy, sandbox, containment, transaction, and redaction layers.
- Displayed commands are never executed through a bypass path.
- Approval tokens are integrity-bound to the exact operation and expire when it changes.
- Reconnects cannot replay stale approval.
- Local IPC validates instance ownership and authentication.
- Transcript, logs, diffs, clipboard/export, browser, MCP, hooks, and artifacts use shared redaction.
- YOLO cannot disable hard-deny, secret, path, sandbox, or protected-verification rules.

## 9. Test requirements

- CLI routing for every invocation form.
- PTY end-to-end launch, prompt input, simple coding task, interruption, resize, recovery, and exit.
- Snapshot/golden tests for views.
- Reducer and event replay property tests.
- Daemon auto-start, reconnect, crash recovery, ownership, and protocol compatibility.
- Windows named-pipe integration tests.
- Approval binding and stale-approval adversarial tests.
- Secret redaction on every rendered/exported surface.
- Worker race and cleanup tests through the TUI.
- Accessibility/plain-output tests.
- Packaging smoke for `medusa --help`, `medusa doctor`, bare `medusa`, and scripted TUI launch/exit.
- Credential-gated live MiniMax PTY tests using simple coding prompts in disposable repositories.

Required interactive coding scenarios:

1. Create a small function and its test in a disposable Rust repository.
2. Fix one intentionally failing test.
3. Rename a symbol and update references.
4. Reject a dangerous shell request and continue safely.
5. Interrupt and resume a session.
6. Review and reject one hunk while accepting another.
7. Run a background test command and inspect its logs.
8. Recover the UI after daemon restart.

Each scenario records transcript, events, diff, verification, exit status, and credential-redacted evidence.

## 10. Delivery policy

Implementation may use multiple reviewable commits and pull requests, but partial functionality must not be merged to `main` as the completed TUI. All work lands on an integration branch until the complete acceptance suite passes. Structural commits must preserve existing headless behavior and release gates.

Recommended workstreams:

1. invocation and reusable TUI shell
2. cross-platform daemon lifecycle and transport
3. event-driven transcript and cancellation
4. composer, history, commands, and completion
5. plans, jobs, and workers
6. diff review, approvals, rewind, and checkpoints
7. memory, skills, hooks, MCP, browser, onboarding
8. accessibility, packaging, interactive E2E, and final release reconciliation

## 11. Acceptance criteria

The program is complete only when:

1. `medusa` launches a usable coding TUI.
2. First-time setup, task submission, progress, review, and verification work entirely inside the TUI.
3. Existing headless commands remain compatible.
4. `medusa-tui` launches the same implementation during deprecation.
5. Daemon auto-start and reconnect work.
6. Linux, macOS, Windows CMD, PowerShell, Windows Terminal, and WSL pass launch/exit and coding-flow tests.
7. Terminal state restores after normal exit, error, panic, and interruption.
8. No UI action bypasses security controls.
9. Sessions, plans, approvals, jobs, and evidence survive restart.
10. The TUI remains responsive during model calls, tools, verification, and background work.
11. All deterministic, adversarial, security, migration, chaos, browser, live-provider, packaging, and cross-platform gates remain green.
12. Documentation and release packages make `medusa` the primary entry point.
13. Workspace coverage reaches the roadmap completion target with meaningful assertions.
