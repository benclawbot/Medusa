# Medusa product quality, usability, performance, and updater specification

## Goal and authority

Make Medusa dependable and easy to use for ordinary conversations and repository work in both the TUI and desktop app. Prioritize reliable updates, preservation of user work, predictable input, understandable progress, accessible navigation, and responsive long sessions. The user requested a full improvement specification and implementation by Luna at High reasoning effort.

Baseline: `main` / `origin/main` at `e3b3c47e24632b4e40fb325fe2a0f13c3f89504d`, fetched and fast-forwarded on 2026-09-06. Earlier inspection at `40ffb00d` remains applicable except for two update-bundle validation scripts changed by PR #1121. The workspace was clean before this specification. Follow root AGENTS.md, docs/CODEX-RELIABILITY.md, and docs/PROTOCOL-VERSIONING.md.

This is the complete implementation backlog from this investigation, not a claim that every possible defect in the repository has been discovered. VERIFIED below means the stated source behavior or observation is established; it does not mean the proposed fix has been implemented or its impact measured. INFERRED requires reproduction or profiling before implementation. Keep the acceptance ledger current rather than marking whole categories complete after one improvement.

## Product experience

The primary loop is: open Medusa, connect once, choose a project if needed, type a request, understand progress, steer or stop, read the result, inspect changes, and resume later. Advanced runtime information should be available on demand. Preserve existing safety controls and expose the reason for decisions in plain language.

Use these official references as interaction guidance, not a requirement to clone every feature or introduce OpenAI dependencies:

- https://learn.chatgpt.com/docs/codex/cli — keyboard-first terminal work and conversation continuation.
- https://learn.chatgpt.com/docs/projects — related conversations organized under projects, with projectless conversations supported.

Already present: desktop projectless chat, provider onboarding, queued follow-ups, stop controls, Markdown/code copy, session inspection, bounded message/work/activity lists, review/recovery docks, TUI background draft writes, deterministic fast mutation preflight, signed updates and rollback. Improve these paths rather than replacing them or proposing them as missing.

## Baseline evidence

- `npm test -- --reporter=dot` in apps/medusa-desktop: PASS, 40 files / 173 tests, 57.60 seconds, on 40ffb00d. Desktop source is unchanged at e3b3c47e.
- `npm run build`: PASS; Vite output JS 281.87 kB (84.50 kB gzip), CSS 128.26 kB (24.42 kB gzip). Build time 34.57 seconds under concurrent test load; not an application performance measurement.
- `python scripts/test-main-update-bundle.py` at e3b3c47e: PASS, 10 tests.
- Rolling workflow run 34020918575: success at e3b3c47e; exact-revision release has CLI and desktop artifacts, manifests, and signatures for the published platforms. Run 34019123064 at 40ffb00d failed. A successful latest publication does not prove an installed application can restart.
- User reports both updaters fail to restart and still show an old version.
- Read-only local inspection: CLI `--version` reports `medusa 1.0.7.1 · main e3b3c47e2463`; desktop install directory `.medusa-update-state` contains `rolled-back`. The leftover installed desktop helper uses a different implementation from current source. Precise historical failure reason is unavailable from its state marker. Do not describe the user report as fully reproduced.
- `cargo test -p medusa-update --all-targets --locked`: PASS, 42 unit tests and 6 integration tests on Windows. This includes disposable helper success/rollback tests, not the installed desktop's historical failure.
- Native desktop interaction, cold startup, screen reader behavior, macOS/Linux installation, full workspace gates, and real provider latency have not been verified in this baseline.

## Non-negotiable constraints

1. Preserve containment, signed manifest trust, exact revision binding, size/hash verification, anti-downgrade policy, credential redaction, checkpoint durability, and rollback.
2. Never install over the user's running CLI/desktop, kill their sessions, modify installed updater scripts, publish releases, or perform a real self-update as part of automated validation. Use disposable installation directories and fixture executables. Report any native action still requiring a real installation test.
3. No force pushes, destructive cleanup, history rewriting, or unrelated refactors. The user subsequently authorized finishing existing GitHub PRs, creating new PRs, and merging to main. Use isolated worktrees/branches, inspect remote state before pushing, and push only attributable reviewed changes. Parent coordinates merges after review and all applicable required checks pass, then verifies main. Source merge permission does not authorize live self-updates.
4. Reuse native facilities and current dependencies. Add a dependency only for a demonstrated requirement with size/license/security review. Do not weaken tests or skip safety gates for speed.
5. Implement related changes in coherent increments. Fix reproducible defects before cosmetic restructuring. Keep backward-compatible DTO/config readers or include explicit migrations.
6. Never store draft secrets in generic localStorage, expose raw provider credentials in diagnostics, or weaken link/path validation to make a UI feature work.

## Required work and acceptance criteria

### U1 — Installed identity and accurate update availability (P1)

VERIFIED: desktop_update.rs::status reports CURRENT_RELEASE_ID, resolves latest main, and sets ready solely from artifact publication. It does not compare installed commit identity. CURRENT_RELEASE_ID is the constant `1.0.7.1`; desktop UI can display the same version before and after different main builds. CLI source_channel has an installed commit comparison, but check-only reports a main update without checking publication.

Implement shared identity vocabulary: release ID, installed source revision, channel, available verified revision, and publication state. Embed desktop source revision reproducibly; handle unknown/non-Git builds explicitly. Display `up to date` when revisions match; disable reinstall as the ordinary action. Do not call unpublished commits available updates. Keep explicit source/main behavior distinguishable from stable release updates.

AC-U1: same SHA, newer published SHA, newer unpublished SHA, unknown installed SHA, offline check, stable channel, and signature failure all have tested distinct outcomes. CLI and desktop show the installed SHA after a successful fixture update. Tests must not assume a SemVer change on every main build.

### U2 — Windows replacement and restart contract (P1)

VERIFIED: windows_install.rs schedules a PowerShell helper and calls process::exit(0) from inside the shared installer. Desktop's later app.exit/progress path is unreachable on successful Windows staging. Restart.detached is not used by the Windows helper. Helper inherits its environment/working directory and kills processes using the target; tests use small fixture programs, not an interactive terminal and packaged WebView lifecycle.

Make staging return a ScheduledUpdate result and leave exit ownership to the caller. Specify and test helper readiness, caller shutdown, replacement, restart arguments, working directory, terminal attachment versus GUI restart, and rollback. Ensure the helper survives the parent. Avoid console flashes for GUI helpers and do not hide the user-facing interactive CLI. Preserve unrelated processes; review the need to stop other sessions before changing it. Replace infinite/unobservable waiting with bounded, observable handoff behavior.

AC-U2: disposable Windows installations test paths with spaces/apostrophes/brackets/non-ASCII, no restart args, quoted args, running target, helper startup failure, swap failure, replacement exit, timeout, and success. An actual terminal fixture remains interactive after restart. The desktop fixture starts in the intended directory. A failed candidate restores a runnable original and retains its failure reason. No fixture touches real installed Medusa binaries.

### U3 — Health acknowledgement and update outcome persistence (P1)

VERIFIED: desktop lib.rs acknowledges health before constructing/running Tauri. CLI acknowledges after first-run/provider preflight but before run_tui. Current helper accepts any nonempty health file. The local desktop state marker is rolled-back without an actionable reason.

Define health as the replacement instance reaching an explicit usable startup milestone, independently of provider/network availability. Tie acknowledgement to the staged update identity/nonce. Desktop should acknowledge after native window/renderer readiness; CLI after terminal/runtime bootstrap is usable, with actionable degraded startup allowed by policy. Retain a redacted structured outcome: target/previous revision, stage, reason, timestamps, rollback result. Surface it on next start and in update settings/CLI diagnostics.

AC-U3: renderer/bootstrap failure cannot commit a healthy update; slow/expired provider auth does not alone roll back a healthy binary; stale/foreign health file is rejected; success, rollback, permission failure, and failed recovery are distinguishable after restart. Preserve compatibility with legacy state markers.

### U4 — Upgrade from legacy installations (P1)

VERIFIED local state proves a rollback, and the retained desktop helper differs from current source. UNKNOWN which historical executable/script caused the user's exact failure.

Reproduce with a disposable old-version fixture or archived helper behavior. Provide a supported, signed recovery path when an old updater cannot bootstrap itself. Detect stale state and explain safe recovery; never automatically overwrite a live installation based solely on a stale marker. Document how to inspect installed revision and last update outcome.

AC-U4: test old state formats, a preexisting backup, a dead helper lock, a live helper lock, and a failed old-to-new restart. Record the exact historical reproduction result or leave it explicitly blocked; do not conflate current helper tests with migration evidence.

### U5 — Publication-aware channel checks (P2)

VERIFIED: both updaters bind selection to latest main; CLI may wait 600 seconds for its artifact. Rolling CI cancels earlier runs, so a selected SHA may never publish. Desktop rejects if main moves between check and apply even when the checked artifact remains valid.

Separate latest source revision from latest verified installable revision. Preserve explicit exact-main requests; for ordinary updates use a documented verified channel pointer or explain pending/failed publication. Pin a checked signed revision throughout download/install. A later main commit must not invalidate a still-valid checked immutable artifact. Add bounded waits and clear retry/cancel guidance.

AC-U5: canceled builds, missing assets, delayed signatures, main advancing, HTTP failure/rate limit, and current successful publication are covered without downgrading signature/revision trust. A check never promises an update that the corresponding installer cannot select.

### D1 — Ignore IME composition Enter (P1)

VERIFIED: App.tsx textarea Enter handler sends without checking composition state. Preserve Chinese/Japanese/Korean text composition and slash completion.

AC-D1: composition Enter never submits; ordinary Enter submits once; Shift+Enter inserts newline; slash completion and composed text work together. Add a regression test before the fix and exercise browser-native composition where feasible.

### D2 — Reject stale asynchronous results after switching runtime/project (P1)

VERIFIED: App polling checks active before awaiting pollRuntime but applies successful events afterward without rechecking. runtime.ts::pollRuntime mutates a global timeline/recovery store before returning. SessionDock refresh/read requests similarly lack generation guards. INFERRED consequence: old task content/status can reach a new view.

Use explicit request/runtime generations or cancellation at both transport publication and UI application boundaries. Apply the same invariant to artifact discovery and session lists/details. Keep the old usable view until the new transition succeeds.

AC-D2: delayed A poll resolves after B starts; A cannot overwrite B messages, busy state, recovery, plan, or artifact. Delayed A session detail/list cannot populate B. An obsolete runtime close must not clear active recovery state. Tests control promise resolution order.

### D3 — Explicit session resume routing (P2)

VERIFIED: runtime.ts::startRuntime consumes a global pending resume ID from localStorage; App's resume callback receives a sessionId but relies on that hidden state. A failed/blocked request can leave that pending ID for a later project open.

Make resume an explicit typed call with project/session identity. Scope any persisted resume intent and clear it atomically on success/cancellation; migrate stale legacy keys safely.

AC-D3: failed resume followed by opening a different project cannot resume the wrong task. Concurrent/duplicate resume requests cannot cross-wire sessions. Startup resume and ordinary new chat remain distinct.

### D4 — Preserve drafts and recover failed sends (P1)

VERIFIED: desktop prompt and attachments live in App state; sending clears them before configuration/submit succeeds. lastRequest provides retry but drafts are not restored after app restart. TUI DraftStore is a useful precedent and already debounces writes.

Persist drafts per conversation/project using a bounded, privacy-aware store. Keep sensitive commands/credentials out of generic persistence. Preserve pending input on failed submission and on project changes. Retain attachment references safely; do not persist large base64 payloads in localStorage. Provide explicit retry/edit/discard for rejected messages; avoid duplicate user messages on retry.

AC-D4: restart, project A/B switching, attachment validation failure, provider configuration failure, submit failure, retry, and discard preserve the right draft without leaking across projects. Limits and cleanup are tested. Successful acceptance clears only that draft.

### D5 — Complete session navigation (P2)

VERIFIED: SessionDock slices sessions to 12 with no continuation control, while backend can return 2,000. It depends on a nonempty project path, despite projectless chat support.

Provide searchable/paginated history, clear active task selection, one-step resume, and access to projectless conversations. Reuse existing session authority for rename/archive if supported; otherwise add backward-compatible metadata rather than destructive deletion. Preserve scroll and selected task across refreshes. Make search/filter and no-results states explicit.

AC-D5: 13th and older tasks are reachable; title/query filtering, project switching, general-chat history, loading/error/retry, and keyboard navigation are tested. Archive has a reversible path. Do not claim full-text search if only titles are searched.

### D6 — Usable links, file references, and Markdown (P2)

VERIFIED: MarkdownMessage safeHref only accepts HTTP(S)/mailto; normal mouse clicks are prevented unless Ctrl is held; macOS Meta-click is not handled. Nested lists are flattened by the custom parser, and fenced code is displayed without syntax highlighting.

Restore conventional link behavior with an explicit safe external opener and validated repository file navigation. Make local path references clickable only through a confined backend route. Preserve safe raw-HTML behavior. Correct nested lists, escaped table separators, and streaming fences using focused fixtures. Add lightweight syntax highlighting only if justified by current dependencies/bundle impact.

AC-D6: mouse/keyboard/macOS modifier activation is predictable; unsafe schemes/path escapes remain blocked; links with parentheses and escaped characters work; representative nested Markdown renders correctly; code copy stays exact during/after streaming.

### D7 — Follow-up, stop, retry, and approval state clarity (P2)

VERIFIED: queued follow-ups already exist. Sending a new turn resets several shared activity/question flags; approval presentation infers intent from label substrings in ApprovalCard.tsx.

Keep queued follow-ups visibly tied to their turn and distinguish accepted/queued/sending/failed/canceling states. Do not mark an active turn idle solely because a follow-up was rejected. Prevent duplicate approval submission; preserve typed decision identity rather than deriving authorization from a friendly label where the protocol supports it. Leave safety authority in the runtime.

AC-D7: active turn plus failed queued submit, stop while submit pending, repeated stop, duplicate approval, stale approval, and retry ordering have behavioral tests. UI never implies cancellation completed before acknowledgement.

### D8 — Accessible, focused desktop interaction (P2)

VERIFIED: focus support exists via useDialogFocus/useDockShell. Source-string mobile-navigation tests do not establish real layout/accessibility. Multiple independently mounted docks and a pointer-only side-panel resize need integration verification.

Use one active modal surface, correct focus return, an accessible composer name, visible keyboard focus, and discoverable shortcuts. Support keyboard panel sizing or a simple accessible size control. Test narrow windows, zoom, reduced motion, high contrast, long text, and screen-reader announcements. Keep routine tool events out of assertive announcements. Add a jump-to-latest control that respects reading older messages.

AC-D8: actual browser tests verify focus trapping/return, Escape, keyboard-only primary flow, no obscured controls at supported minimum size/200% zoom, and persistent scroll position while streaming. Accessibility checks supplement, not replace, interaction tests.

### D9 — Simplify onboarding, empty states, and error recovery (P2)

VERIFIED: onboarding has provider/auth/model/permission steps; general-chat empty-state suggestions are repository-specific; DesktopErrorBoundary reports failure but has no actionable recovery control.

Prefer a working existing provider/model configuration, keep advanced routes/settings optional, and explain permission choices plainly. Tailor starter prompts to general chat versus a repository. Make error banners actionable with retry/edit/reconnect as appropriate. Add a renderer recovery action that preserves durable work and does not falsely promise the backend is healthy.

AC-D9: fresh setup, already configured account, OAuth failure, missing provider, general chat, repository chat, offline/degraded state, and renderer failure have distinct tested next actions. Do not relax permission defaults.

### P1 — TUI rendering cost and idle work (P2)

VERIFIED: portable session loop requests a frame every daemon status poll (250 ms); render_frame builds transcript_lines before taking the visible slice. INFERRED impact grows with history size.

Measure render time/allocation and idle work first. Cache immutable transcript layout by width/content revision or render only the necessary visible range; invalidate correctly on resize, expansion, and new text. Redraw for visible state changes. Preserve selection/scrollback behavior and platform-specific terminal semantics.

AC-P1: benchmarks cover 100/1,000/10,000 transcript entries, narrow/wide resize, streaming and idle. Show before/after results; avoid a full history rewrap on unchanged idle frames. Selection and scrollback regressions pass.

### P2 — Remove synchronous daemon observation from terminal input loop (P1/P2)

VERIFIED: DaemonMonitor::poll synchronously calls client.request(List) from the presentation loop, separately from the runtime background event poller. INFERRED: a slow daemon can delay keyboard handling.

Move status observation onto an existing background/event path or a bounded worker with cached snapshots. Avoid duplicate polling/lifecycle ownership and preserve transition reporting. Do not start an additional competing daemon supervisor.

AC-P2: an intentionally delayed/unresponsive daemon cannot block typing, Ctrl-C, or resize; failure transition emits once and recovery is reflected; background work terminates cleanly. Include a timing-controlled regression test.

### P3 — Desktop rendering and polling budgets (P2)

VERIFIED: desktop polls every 80 ms while busy / 750 ms idle; transcript displays an initial 120 messages with incremental expansion; App owns composer, transcript, settings, work log, and lifecycle state. MarkdownMessage is already memoized; current runtime activity reducer already builds an index lazily. Do not claim those optimizations are missing.

Profile before optimizing. Separate unrelated render ownership so typing does not rebuild the work log/transcript shell. Batch events and preserve stable selectors. Bound rendered work-log/history rows while keeping older content accessible. Add visibility-aware polling/backoff or push events only if measurements justify the transport change. Preserve replay/ack ordering.

AC-P3: benchmark typing and streaming with long sessions; count render/IPC work for idle, active, background and reconnect. No busy-loop polling, missed terminal event, or stuck busy state. Report measurements without inventing speedup percentages.

### P4 — Indexed and bounded session reads (P2)

VERIFIED: desktop sessions.rs scans and parses session JSON files before sorting/truncating, and reads full message JSON before truncating. Presentation caps do not bound the initial disk/parsing work.

Add incremental metadata/index reuse and cursor pagination at the session authority, or another minimal design demonstrated by benchmarks. Preserve fallback-root behavior, malformed-session handling, and compatibility. Avoid synchronous expensive disk work on GUI dispatch paths.

AC-P4: 10/1,000/10,000 session benchmark fixtures; bounded page results and message reads; corrupt item isolation; stable ordering; updates invalidate the right metadata; no lost sessions across old/new formats.

### T1 — Terminal Unicode and editing (P2)

VERIFIED: wrap_to_width and several render helpers count Unicode scalar values, not terminal cell widths; composer cursor moves by char boundaries and lacks Home/End/Delete/word movement. Up/Down are routed to command selection even for ordinary multiline input.

Use consistent display-cell and grapheme handling across wrapping, cursor movement, selection/copy, cropping and tables. Add standard editing keys and multiline vertical navigation, preserving command picker behavior when active. Resolve shortcut conflicts explicitly (for example existing Ctrl-E activity details).

AC-T1: wide CJK, combining marks, emoji sequences, tabs, multiline text, deletion and resize have tests; rendered rows fit terminal cells; copy yields original text; Home/End/Delete/word and command picker interactions are documented and tested on supported terminals.

### T2 — TUI discoverability and task continuity (P2)

Reuse existing model/session/command pickers. Improve contextual help, visible send/newline/stop hints, searchability and prompt history without clutter. Preserve unfinished input when switching tasks. Show actionable reconnect/auth/update failures and distinguish working/waiting/queued/stopped.

AC-T2: a keyboard-only user can discover help, select a model, submit multiline text, stop, find a saved task and resume without memorizing an ID. Test empty history, unavailable provider, slow daemon and narrow terminal. Retain native scrollback/selection conventions.

### Q1 — Explicit frontend ownership and maintainable styles (P2)

VERIFIED: main.tsx mounts several sibling docks. DesktopTimelineBridge polls for a DOM mount for only 40 x 100 ms; completing onboarding after that can miss the mount. DesktopUpdateControl observes the whole document subtree to locate .settings-form. Many overlapping theme/override CSS files are imported in order.

Replace DOM discovery with explicit React slots/refs/context as needed. Split App by cohesive ownership after behavioral coverage exists. Consolidate only the touched duplicate style authority into shared tokens/components; avoid a global cosmetic rewrite. Remove obsolete bridges only after callers/tests show replacement.

AC-Q1: onboarding lasting over four seconds then starting a planned task still shows the plan; leaving/reentering chat remounts controls correctly; update controls mount in their intended settings area only; no body-wide observer is required for known React children. Preserve visual and interaction regressions.

### Q2 — Contract-driven frontend state (P2)

Use typed lifecycle/decision models where they prevent proven state bugs. Consolidate duplicated timeline semantics only after inspecting live callers; do not delete an alternate reducer solely because it looks unused. Scope stores by runtime/session and make bounded retention explicit. Keep DTO migration compatible.

AC-Q2: duplicate/replayed/out-of-order events and runtime changes yield deterministic state; test the production reducer, not only a parallel unused model; no invisible cross-task global authority remains for resume/recovery.

### Q3 — Production observability and realistic quality gates (P1/P2)

Record redacted update stages and relevant performance phases using existing diagnostics. Extend acceptance coverage beyond source-string assertions and mocked React IPC: packaged native startup, update/restart, daemon reconnect, long sessions, narrow windows and keyboard flows. CI must test the artifact it publishes. Do not substitute a static script match for a process-lifecycle test.

AC-Q3: updater failures retain useful reasons, packaged-window readiness is tested, browser/PTY checks are runnable and documented, and each changed acceptance criterion maps to an executed test or explicit platform limitation. Never label all platforms verified from Windows results.

### P5 — Core runtime latency opportunities (measurement first, P2)

VERIFIED: deterministic fast preflight already exists in coordination/multi_agent_coordinator.rs. Do not remove review, containment or durability gates to reduce latency.

Measure startup-to-input, submit-to-ack, preflight, provider first visible output, tool scheduling, verification and final persistence. Inspect production callers before changing caching, worker startup or HTTP clients. Reuse valid evidence/cache entries by authority key; avoid speculative parallelism. Implement only bottlenecks supported by measurements, otherwise retain a documented baseline and future threshold.

AC-P5: a repeatable offline scenario corpus reports phase times for simple read-only work, localized edit, broader edit, failure/retry and resume. Any optimized path preserves existing safety and equivalence tests. Provider/network timing remains UNKNOWN without an authorized controlled live run.

## Codebase-wide investigation additions

The investigation includes production entry paths and direct dependencies beyond the frontends. This coverage map describes inspection, not full certification of every file:

| Area | Inspected entry points / evidence | Required follow-through |
|---|---|---|
| Runtime/orchestration | runtime/lib.rs::run_prompt, production_orchestrator, multi_agent_coordinator, attachment/session | P5 phase benchmarks; preserve existing deterministic fast lane, session config binding and review gates |
| Provider transport | provider/http.rs, openai_transport.rs, openai_streaming.rs, streaming.rs | R2 bounded streaming and cancellation; shared clients/runtime already exist |
| Tool execution | agent/tools/shell.rs, tool_scheduler.rs, tool_orchestration.rs | R7 validate actual budget authority and avoid cosmetic scheduler claims |
| Session durability/replay | agent/journal.rs, runtime/checkpoint_store.rs, attachment/session | R1 append/replay scalability without weakening commit boundaries |
| Context/memory | context-retrieval/lib.rs, memory/retrieval.rs, memory/index.rs, intelligence/index.rs | R3 cache identity, R4 memory/index cost; incremental code indexing already exists |
| Daemon/processes | daemon/server_base.rs, transport.rs, lifecycle entry, TUI observer | P2 plus R8 bounded IPC responses and cancellation checks |
| Configuration | config/configuration_state.rs; frontend configuration callers | R6 lock ownership; retain optimistic revision checking and redaction |
| Browser/external tools | browserd/proxy.rs, browser client network-policy entry, github repository operation backend | R5 proxy lifecycle; GitHub backend already uses bounded command output and structured temporary request bodies |
| Packaging/production | ci/desktop/rolling/verified update workflows; live PR checks and release assets | U1–U5/Q3 plus PR1122 repair; do not confuse artifact publication with restart readiness |
| TUI/desktop | input/render/session, App/runtime/session/onboarding/Markdown/docks | D/P/T/Q requirements above |

Less deeply inspected domains include voice/realtime, Telegram, learning/evaluation, extension installation, time-travel coordination and every platform-specific containment path. Do not invent defects in them or label the entire repository exhaustively verified. Their integration contracts and affected tests must run if the implementation changes shared runtime/protocol/storage behavior. A future security audit is separate from this production-quality review.

### R1 — Journal append/replay scalability and bounded retention (P1/P2)

VERIFIED in agent/journal.rs: append_payload_committed reads the full journal through read_journal, clones the session, and appends both an event and a complete session snapshot containing prior events. read_journal reads the entire file into memory. Appends invalidate the read cache. This repeats growing history during persistence, beyond the frontend history caps. INFERRED impact: growing disk amplification, append latency and restart memory for long sessions. A 32 MiB per-frame cap can eventually reject a growing full snapshot.

Measure fixed-size event sequences at increasing lengths and record file growth, append/replay time and memory. Implement a versioned incremental journal/checkpoint scheme or another minimal compatible optimization that preserves atomic event+materialization semantics, chain integrity, torn-tail recovery, idempotency, canonical terminal persistence and session attachment. Bound cache retention by bytes/entries and account for completed sessions. Never simply omit the snapshot without proving equivalent crash recovery.

AC-R1: compare at least 100/1,000/10,000 small events; near-frame-limit session behavior is explicit; old journal formats replay; cancellation/crash between every write boundary cannot acknowledge uncommitted state; corruption and concurrent attachment tests pass. Publish measured improvement and schema/rollback evidence in a dedicated PR.

### R2 — Bounded provider streaming and prompt cancellation (P1/P2)

VERIFIED: provider/openai_transport.rs::SseDecoder grows pending and data buffers without an explicit byte cap; it drains a Vec from the front for each line. stream completion transport uses mpsc and a scoped worker; accumulators retain response text/tool fragments. Non-streaming HTTP bodies already have explicit limits in http.rs. INFERRED: oversized/no-newline SSE and slow consumers can consume excessive memory; a sink failure can wait for a still-live transport worker unless cancellation is propagated.

Add explicit configurable/internal limits for a line/event, accumulated response/tool arguments and queued events. Use efficient incremental scanning and bounded backpressure. Propagate sink failure/cancel to the transport and join promptly. Apply analogous limits to other active provider transports after inspecting their callers. Preserve incremental UTF-8, split CRLF, usage accounting, private-reasoning filtering and exactly-once tool readiness.

AC-R2: stalled socket, consumer failure, fast producer/slow consumer, over-limit line/event/arguments, split Unicode and valid large output have offline mock-server tests. Cancellation finishes within a bounded tested interval without leaked threads or waiting for the entire 120-second request timeout. Report retained-byte bounds. Do not claim a remote vulnerability without a separate validated attack path.

### R3 — Context memo identity and public-contract correctness (P2)

VERIFIED: context-retrieval/lib.rs::memo_key hashes query text and required IDs by concatenation without field framing, and ledger entries only by ID/sequence. retrieve_cached returns hits before query/ledger validation. The public RetrievalMemo currently has no production caller found outside its module/tests, so this is a public API correctness defect candidate, not a demonstrated live runtime speed bottleneck.

Add collision/changed-ledger tests (including distinct valid ledgers sharing IDs/sequences and changed content). Hash canonical framed query/ledger authority or use a stable full content/revision fingerprint with appropriate validation. Do not wire the memo into production just to justify it; integrate only with evidence of benefit.

AC-R3: different required-ID sets and different ledger content cannot share a cached result; invalid requests cannot bypass validation via a hit; default/new capacity semantics are consistent; memoized and uncached results agree for generated inputs. Scope the PR honestly to the public contract unless a real caller is subsequently found.

### R4 — Memory index efficiency and rebuild resilience (P2)

VERIFIED: memory/retrieval.rs::search loads canonical documents, filters/scores/sorts all before truncate. memory/index.rs::rebuild_index removes the previous SQLite index and inserts records/tags/sources individually without an explicit surrounding transaction. SQLite exists as a dependency; do not introduce another database. UNKNOWN measured production cost and rebuild call frequency until traced.

Trace runtime retrieval and rebuild callers, benchmark representative document sets, and use transaction/batched rebuild with atomic publication of a complete derived index. Preserve canonical Markdown authority and deterministic score/expiry/confidence filtering. Use index-assisted candidate selection only when equivalence is proven; handle external edits and stale/corrupt indexes safely.

AC-R4: rebuild interruption leaves a recoverable previous/complete index; empty and malformed document cases are explicit; indexed and canonical search agree; changes/expiry invalidate appropriately; record 100/1,000/10,000 document measurements. Avoid rebuilding the whole index for each small metadata update where an incremental equivalent exists.

### R5 — Browser proxy resource ownership (P2)

VERIFIED: browserd/proxy.rs spawns a listener thread and one thread per accepted connection; inspected Proxy value exposes the address without an evident stop/join owner. Review all lifetime owners before asserting a production leak. Network address resolution/pinning policy already exists and must be preserved.

Introduce bounded connection admission and explicit shutdown/cancellation ownership if not provided by the caller. Ensure tunnel half-close/timeouts and listener cleanup allow browser sessions to close without hanging processes. Keep private-network and DNS-rebinding protections intact.

AC-R5: repeated start/stop, idle connected client, saturated admission, tunnel failure, shutdown during connect and peer half-close are deterministic tests. Thread/socket counts return to baseline. A bounded failure produces a useful error rather than silent dropped work.

### R6 — Configuration writer lock identity (P2)

VERIFIED: configuration_state.rs::lock_is_stale uses modification age >=30 seconds. Inspect acquire/guard drop to reproduce whether a still-live writer exceeding that age can have its lock reclaimed. Do not assume rare timing means impossible.

Use ownership-aware stale detection or an OS-backed advisory lock consistent with existing storage facilities. Never reclaim a live writer solely by elapsed time; ensure a departing old owner cannot remove a new owner's lock. Preserve revision/CAS semantics and redacted change events.

AC-R6: slow live writer, crashed writer, PID reuse or unavailable identity, concurrent updates and guard cleanup are tested with controlled clocks/fixtures. One successful commit increments revision exactly once and stale expected revisions still reject.

### R7 — Tool execution budget authority and overhead (P2)

VERIFIED: tools/shell.rs creates ExecutionBudget::for_turn(1) per run_validated invocation, so that instance cannot enforce its documented whole-turn repeated-call/count/output bounds across calls. Other engine/tool-control budgets may already provide the real authority; inspect before changing behavior. tool_orchestration::registry constructs owned metadata per request, and shell telemetry has multiple formatting/persistence stages.

Map actual runtime budget/loop-guard ownership. Consolidate misleading per-call telemetry with the real turn-scoped authority or correct its semantics, preserving legitimate re-reads after mutations and verification retries. Measure metadata/telemetry overhead before caching/static-table changes. Keep shell results uncached unless an explicit safe cache policy proves otherwise (registry currently marks shell_run non-cacheable).

AC-R7: successive calls enforce the documented aggregate limit at the production caller; repeated unchanged loops are bounded while a changed repository/verification retry remains allowed; no double charge for a single call; failure output stays available and credentials remain redacted. Remove redundant work only with behavioral equivalence evidence.

### R8 — Bounded daemon response ingestion (P2)

VERIFIED: daemon/server_base.rs::DaemonClient::request uses BufReader::read_line into a String before deserializing ResponseEnvelope; no response byte cap is visible in that client path. Server-side/request limits and local transport authentication do not inherently bound that allocation.

Verify the active wrapper and existing protocol message limits, then apply a bounded response framing reader consistent with large valid replay/attachment responses. Consider paginated large projections rather than raising an unlimited buffer. Keep reconnect, version checks, authenticated IPC and durable replay acknowledgements intact.

AC-R8: oversized response/no newline, truncated JSON, version mismatch, normal large replay and slow peer have bounded memory/time outcomes with useful errors. Cancellation/stop requests stay responsive when large projection reads are pending.

## Existing GitHub work and non-overlap rules

On 2026-09-06, open PR **#1122**, `Implement measured desktop and TUI hardening spec`, branch `codex/medusa-spec-p2-p5-t1-t2-q1-q3`, head `7a76859197db3a9fc22e9a1499cb825a6a38200c`, targets main. Recheck its head before editing/pushing: another connector may resume.

Observed changed areas: TUI daemon_status/input with new legacy/text_cells modules; desktop sessions/paging/runtime wakeup; App/AppLegacy and runtime/runtimeLegacy; main mounts and DesktopUpdateControl. These changes are not yet reviewed or merged. Do not duplicate these features in another PR or accept the body as completion evidence.

Observed CI at that head: all three rust-adapter jobs fail; macOS bundle-smoke fails a frontend App test expecting startRuntime('/repo'); workspace quality is canceled and downstream jobs skipped. Frontend on Linux and several other checks pass. Inspect all complete failure logs and uploaded rustfmt/clippy diagnostics before fixing; do not weaken the resume test or rerun until green without finding the cause.

Assign one agent exclusive ownership of continuing #1122, reconciling its actual diff against D/P/T/Q criteria and opening follow-on PRs for missing frontend/TUI work only when they do not duplicate its branch. Assign a separate updater agent U1–U5 and a separate core agent R1–R8/P5. All agents use isolated worktrees; coordinate shared desktop lib.rs/CI changes. Parent owns this specification, independent review and merge ordering. Merge is authorized only after applicable checks and review pass.

## Implementation order and ownership

The user now explicitly authorizes multiple Luna High subagents, finishing existing PRs and opening remaining PRs. Use the three non-overlapping workstreams above, with separate worktrees and concise per-PR evidence. Parent owns independent review, baseline evidence and this specification; agents maintain separate implementation ledgers. No additional recursive delegation is needed.

1. U1–U4: identity, handoff/health, diagnostics, legacy updater reproduction. Request parent review of updater diff before expanding scope.
2. D1–D4, D7: composition, stale event isolation, explicit resume, drafts/failed sends and lifecycle correctness.
3. D5–D6, D8–D9, Q1–Q2: navigation, readable responses, accessibility, mounting and focused code simplification.
4. P1–P4, T1–T2: measured UI performance and terminal ergonomics.
5. U5, P5, Q3: publication policy, core phase measurements and complete production certification.

Each increment must include failing-before/passing-after regression evidence where practical, focused tests, relevant documentation, final diff review and a concise checkpoint. Keep the program usable between increments. A blocked platform test does not justify pretending that requirement is complete. Complete independent requirements while documenting true blockers.

## Verification commands

Run targeted tests first, then all applicable project gates before declaring implementation complete:

```text
cargo test -p medusa-update --all-targets --locked
cargo test -p medusa-cli --bin medusa --locked update_
cargo test -p medusa-tui --all-features --locked
python scripts/test-main-update-bundle.py
python scripts/check-desktop-version-sync.py --root . --self-test
cd apps/medusa-desktop
npm test
npm run typecheck
npm run build
cd ../..
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-features --locked
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --locked --no-deps
cargo deny check advisories sources
cargo audit
cargo fmt --manifest-path apps/medusa-desktop/src-tauri/Cargo.toml -- --check
cargo clippy --manifest-path apps/medusa-desktop/src-tauri/Cargo.toml --all-targets --locked -- -D warnings
cargo test --manifest-path apps/medusa-desktop/src-tauri/Cargo.toml --locked
```

Use PowerShell environment assignment for RUSTDOCFLAGS on Windows. Follow any additional affected workflow checks. Run native restart tests in disposable fixture locations, plus browser/terminal integration checks introduced above. No live provider calls are necessary for the offline correctness gates. Record actual commands, counts, failures and omissions; do not run a live self-update as a substitute for tests.

Suggested performance goals (targets, not baseline claims): local input feedback within 50 ms p95, ordinary idle presentation causing no repeated full-history layout, no UI-thread filesystem/network wait, and no lost text/events across lifecycle changes. Calibrate benchmark hardware and thresholds before making CI flaky with wall-clock assertions.

## Rollback and open evidence

Keep source rollback straightforward by coherent increments; no persisted schema change without compatibility/migration coverage. Installed update rollback must retain the original until meaningful health confirmation. New draft/history metadata must be recoverable by legacy readers or versioned explicitly. Never delete user sessions to repair a UI problem.

Outstanding evidence: precise legacy desktop rollback reason; real interactive Windows restart continuity; native desktop renderer readiness; cross-platform packaged replacement (including macOS bundle/signing semantics); production latency profiles; full workspace validation. Parent and implementation agent must update this list and the ledger as evidence arrives.

## Acceptance ledger

Initial state: implementation criteria NOT YET VERIFIED. Some P/T/Q work exists in unmerged PR1122 and must be mapped individually after review. Baseline observations above are VERIFIED only at their stated scope. For each U/D/P/T/Q/R identifier record: source changes, regression/check, result, review result, remaining limitation. Final status must be PARTIALLY VERIFIED or BLOCKED while required checks or material acceptance criteria remain unmet.

## Final integrated-main test sweep (explicit user requirement)

After all reviewed implementation PRs merge, fetch and record the resulting main SHA. Run the complete workspace gates above, the complete desktop frontend and native adapter suites, and dispatch all applicable full CI/acceptance workflows that support manual dispatch. For workflows without dispatch, verify runs attached to that exact main commit. Inspect coverage, adversarial regressions, migration/chaos, browser, package/platform, documentation, dependency-policy and production acceptance checks required by affected areas. Record credential-gated or unavailable jobs explicitly; never bypass their gates or invent credentials.

Collect every failed job/test and its complete diagnostic output before choosing repairs. Group shared/cascading causes, fix them coherently, run focused regressions, merge reviewed fixes, then rerun the complete integrated sweep against the new main SHA. Canceled, skipped-required, missing, and earlier-commit checks are not success. Do not stop at the first green platform or describe targeted tests as all tests. Preserve test expectations and investigate flakiness rather than rerunning until green.
