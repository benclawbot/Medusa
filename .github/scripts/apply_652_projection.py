from pathlib import Path


def replace_once(text: str, old: str, new: str, label: str) -> str:
    if text.count(old) != 1:
        raise SystemExit(f"expected exactly one {label}, found {text.count(old)}")
    return text.replace(old, new, 1)

# Move the deterministic projection into medusa-protocol.
daemon_path = Path("crates/medusa-daemon/src/telegram/projection.rs")
projection = daemon_path.read_text()
projection = replace_once(
    projection,
    "//! user-visible presentation data for Telegram and other daemon frontends.",
    "//! user-visible presentation data for every frontend.",
    "projection module documentation",
)
projection = replace_once(
    projection,
    '''use medusa_protocol::{
    EventEnvelope, EventPayload,
    frontend::{
        FRONTEND_PROTOCOL_VERSION, FrontendEvent, FrontendEventEnvelope, PresentationActivity,
        PresentationActivityKind, PresentationApproval, PresentationLifecycle,
        PresentationPlanStep, PresentationQuestion, PresentationQuestionOption, PresentationWorker,
    },
};''',
    '''use crate::{
    EventEnvelope, EventPayload,
    frontend::{
        FRONTEND_PROTOCOL_VERSION, FrontendEvent, FrontendEventEnvelope, FrontendKind,
        PresentationActivity, PresentationActivityKind, PresentationApproval,
        PresentationLifecycle, PresentationPlanStep, PresentationQuestion,
        PresentationQuestionOption, PresentationWorker,
    },
};''',
    "projection imports",
)
projection = replace_once(
    projection,
    '''pub fn project_event(
    event: &EventEnvelope,
    presentation_cursor: u64,
) -> Option<FrontendEventEnvelope> {''',
    '''pub fn project_event(
    event: &EventEnvelope,
    presentation_cursor: u64,
    frontend: FrontendKind,
) -> Option<FrontendEventEnvelope> {''',
    "projection signature",
)
projection = replace_once(
    projection,
    'event_id: format!("{}:telegram", event.event_id),',
    'event_id: format!("{}:{}", event.event_id, frontend_label(frontend)),',
    "frontend event identity",
)
projection = replace_once(
    projection,
    '''fn lifecycle_for_frontend(event: &FrontendEvent) -> PresentationLifecycle {''',
    '''fn frontend_label(frontend: FrontendKind) -> &'static str {
    match frontend {
        FrontendKind::Tui => "tui",
        FrontendKind::Desktop => "desktop",
        FrontendKind::Telegram => "telegram",
        FrontendKind::Headless => "headless",
        FrontendKind::Other => "other",
    }
}

fn lifecycle_for_frontend(event: &FrontendEvent) -> PresentationLifecycle {''',
    "frontend identity helper",
)
projection = replace_once(
    projection,
    "use medusa_protocol::{Actor, EventEnvelope, EventPayload};",
    "use crate::{Actor, EventEnvelope, EventPayload};",
    "projection test imports",
)
projection = replace_once(
    projection,
    'let projected = project_event(&source, 1).expect("projection");',
    'let projected = project_event(&source, 1, FrontendKind::Telegram).expect("projection");',
    "assistant projection test",
)
for cursor, label in [("1", "plan"), ("2", "question"), ("3", "team")]:
    projection = replace_once(
        projection,
        f'''            {cursor},
        )
        .expect("{label}");''',
        f'''            {cursor},
            FrontendKind::Telegram,
        )
        .expect("{label}");''',
        f"{label} projection test",
    )
projection = replace_once(
    projection,
    '''    #[test]
    fn secret_like_tool_arguments_are_not_projected() {''',
    '''    #[test]
    fn frontend_identity_is_scoped_without_changing_payload() {
        let source = event(EventPayload::RuntimeTurnFinished);
        let tui = project_event(&source, 4, FrontendKind::Tui).expect("tui");
        let desktop = project_event(&source, 4, FrontendKind::Desktop).expect("desktop");
        assert_eq!(tui.event, desktop.event);
        assert_eq!(tui.cursor, desktop.cursor);
        assert!(tui.event_id.ends_with(":tui"));
        assert!(desktop.event_id.ends_with(":desktop"));
    }

    #[test]
    fn secret_like_tool_arguments_are_not_projected() {''',
    "frontend identity test",
)
projection = replace_once(
    projection,
    '''            1,
        )
        .expect("projection");
        let FrontendEvent::Activity(activity)''',
    '''            1,
            FrontendKind::Telegram,
        )
        .expect("projection");
        let FrontendEvent::Activity(activity)''',
    "secret projection test",
)
protocol_projection = Path("crates/medusa-protocol/src/frontend/projection.rs")
protocol_projection.write_text(projection)

daemon_path.write_text('''//! Telegram compatibility wrapper over the shared frontend projection authority.

use medusa_protocol::{
    EventEnvelope,
    frontend::{FrontendEventEnvelope, FrontendKind},
};

pub fn project_event(
    event: &EventEnvelope,
    presentation_cursor: u64,
) -> Option<FrontendEventEnvelope> {
    medusa_protocol::frontend::project_event(
        event,
        presentation_cursor,
        FrontendKind::Telegram,
    )
}
''')

protocol_mod = Path("crates/medusa-protocol/src/frontend/mod.rs")
text = protocol_mod.read_text()
text = replace_once(text, "mod event;\n", "mod event;\nmod projection;\n", "projection module registration")
text = replace_once(
    text,
    "pub const FRONTEND_PROTOCOL_VERSION: crate::ProtocolVersion = CURRENT_PROTOCOL_VERSION;\n",
    "pub const FRONTEND_PROTOCOL_VERSION: crate::ProtocolVersion = CURRENT_PROTOCOL_VERSION;\n\npub use projection::project_event;\n",
    "projection export",
)
protocol_mod.write_text(text)

runtime_frontend = Path("crates/medusa-runtime/src/frontend.rs")
runtime_frontend.write_text('''//! Canonical frontend event delivery over the durable session journal.
//!
//! Runtime workers may emit process-local wakeups and presentation hints, but user-facing
//! frontends consume the versioned protocol projected from committed journal events. This keeps
//! replay, ordering, verification, and terminal state identical across CLI and remote clients.

use std::{collections::VecDeque, path::PathBuf};

use medusa_agent::session_browser::replay_events;
use medusa_protocol::frontend::{project_event, FrontendEventEnvelope, FrontendKind};

use crate::RuntimeError;

/// Cursor-bearing projection of one authoritative runtime session for one frontend kind.
pub struct CanonicalFrontendEventStream {
    repo: PathBuf,
    frontend: FrontendKind,
    session_id: Option<String>,
    journal_cursor: u64,
    pending: VecDeque<FrontendEventEnvelope>,
}

impl CanonicalFrontendEventStream {
    #[must_use]
    pub fn new(repo: PathBuf, frontend: FrontendKind) -> Self {
        Self {
            repo,
            frontend,
            session_id: None,
            journal_cursor: 0,
            pending: VecDeque::new(),
        }
    }

    /// Resumes presentation after an acknowledged canonical journal cursor.
    pub fn resume(&mut self, session_id: impl Into<String>, after_cursor: u64) {
        self.session_id = Some(session_id.into());
        self.journal_cursor = after_cursor;
        self.pending.clear();
    }

    /// Returns the next shared frontend event, replaying committed journal state as needed.
    pub fn try_event(
        &mut self,
        session_id: &str,
    ) -> Result<Option<FrontendEventEnvelope>, RuntimeError> {
        if self.session_id.as_deref() != Some(session_id) {
            self.resume(session_id.to_owned(), 0);
        }
        if let Some(event) = self.pending.pop_front() {
            return Ok(Some(event));
        }

        let events = replay_events(&self.repo, session_id, self.journal_cursor)
            .map_err(RuntimeError::agent)?;
        for event in events {
            if event.sequence <= self.journal_cursor {
                return Err(RuntimeError::InvalidCommand(format!(
                    "frontend journal sequence {} did not advance past cursor {}",
                    event.sequence, self.journal_cursor
                )));
            }
            self.journal_cursor = event.sequence;
            if let Some(projected) = project_event(&event, event.sequence, self.frontend) {
                self.pending.push_back(projected);
            }
        }
        Ok(self.pending.pop_front())
    }

    #[must_use]
    pub const fn journal_cursor(&self) -> u64 {
        self.journal_cursor
    }
}
''')

runtime_lib = Path("crates/medusa-runtime/src/lib.rs")
text = runtime_lib.read_text()
text = replace_once(text, "pub mod execution_history;\n", "pub mod execution_history;\npub mod frontend;\n", "runtime frontend module")
runtime_lib.write_text(text)

cli_path = Path("crates/medusa-cli/src/main.rs")
text = cli_path.read_text()
text = replace_once(
    text,
    "use medusa_runtime::{prompt::PromptDraft, RuntimeController, RuntimeEvent};",
    '''use medusa_protocol::frontend::{FrontendEvent, FrontendKind};
use medusa_runtime::{
    frontend::CanonicalFrontendEventStream, prompt::PromptDraft, RuntimeController, RuntimeEvent,
};''',
    "CLI frontend imports",
)
text = replace_once(
    text,
    "drain_headless_runtime(&runtime, approval_policy.as_ref())",
    "drain_headless_runtime(&runtime, &repo, approval_policy.as_ref())",
    "headless run call",
)
text = replace_once(
    text,
    "drain_headless_runtime(&runtime, None)",
    "drain_headless_runtime(&runtime, &repo, None)",
    "headless resume call",
)
start = text.index("fn drain_headless_runtime(")
end = text.index("fn request_daemon_shutdown", start)
new_function = '''fn drain_headless_runtime(
    runtime: &RuntimeController,
    repo: &Path,
    approval_policy: Option<&HeadlessApprovalPolicy>,
) -> MedusaResult<()> {
    let mut stream = CanonicalFrontendEventStream::new(repo.to_path_buf(), FrontendKind::Headless);
    let mut automatically_answered_question = false;
    loop {
        let runtime_event = runtime.try_event().map_err(runtime_error)?;
        match runtime_event {
            Some(RuntimeEvent::Question(question)) => {
                let Some(policy) = approval_policy else {
                    continue;
                };
                match policy.matches(&question) {
                    ApprovalMatch::Approved(command) => {
                        println!("approved allowlisted command: {command}");
                        runtime
                            .submit(PromptDraft {
                                text: "approve".to_owned(),
                                ..PromptDraft::default()
                            })
                            .map_err(runtime_error)?;
                        automatically_answered_question = true;
                    }
                    ApprovalMatch::Missing(command) => {
                        return Err(MedusaError::new(
                            ErrorCode::PolicyDenied,
                            ErrorCategory::Policy,
                            format!(
                                "headless approval denied for `{command}` because it is not listed in {}. Add the exact command and rerun with `medusa run --non-interactive --approve-allowlist {} <objective>`.",
                                policy.source().display(),
                                policy.source().display()
                            ),
                        ));
                    }
                    ApprovalMatch::NotApproval => {}
                }
            }
            Some(RuntimeEvent::Failed(error)) if runtime.active_session_id().is_none() => {
                return Err(MedusaError::new(
                    ErrorCode::DependencyUnavailable,
                    ErrorCategory::Execution,
                    error,
                ));
            }
            _ => {}
        }

        let Some(session_id) = runtime.active_session_id() else {
            std::thread::yield_now();
            continue;
        };
        let mut emitted = false;
        while let Some(envelope) = stream.try_event(&session_id).map_err(runtime_error)? {
            emitted = true;
            match envelope.event {
                FrontendEvent::Started if automatically_answered_question => {
                    automatically_answered_question = false;
                }
                FrontendEvent::AssistantTextDelta { text }
                | FrontendEvent::AssistantInterim { text } => println!("{text}"),
                FrontendEvent::Activity(activity) => {
                    println!("{}: {}", activity.title, activity.details.join("; "));
                }
                FrontendEvent::Notice { title, details, .. } => {
                    println!("{title}: {}", details.join("; "));
                }
                FrontendEvent::Question(question) => {
                    if automatically_answered_question {
                        continue;
                    }
                    return Err(MedusaError::new(
                        ErrorCode::DependencyUnavailable,
                        ErrorCategory::Execution,
                        format!(
                            "agent is waiting for user input, which headless execution cannot provide: {}. For an approval prompt, create an allowlist and rerun with `medusa run --non-interactive --approve-allowlist .medusa/approve.txt <objective>`; otherwise use the interactive terminal.",
                            question.prompt
                        ),
                    ));
                }
                FrontendEvent::ApprovalRequired(approval) => {
                    if automatically_answered_question {
                        continue;
                    }
                    return Err(MedusaError::new(
                        ErrorCode::DependencyUnavailable,
                        ErrorCategory::Execution,
                        format!(
                            "agent requires approval for {} ({}) and headless execution has no matching allowlist decision",
                            approval.action, approval.scope
                        ),
                    ));
                }
                FrontendEvent::Completed { summary } => {
                    if let Some(summary) = summary {
                        println!("session {session_id} completed: {summary}");
                    } else {
                        println!("session {session_id} completed");
                    }
                    return Ok(());
                }
                FrontendEvent::TurnFinished => return Ok(()),
                FrontendEvent::Cancelled { .. } => {
                    return Err(MedusaError::new(
                        ErrorCode::DependencyUnavailable,
                        ErrorCategory::Execution,
                        "agent execution was cancelled",
                    ));
                }
                FrontendEvent::Failed { message, .. } => {
                    return Err(MedusaError::new(
                        ErrorCode::DependencyUnavailable,
                        ErrorCategory::Execution,
                        message,
                    ));
                }
                _ => {}
            }
        }
        if !emitted {
            std::thread::yield_now();
        }
    }
}

'''
text = text[:start] + new_function + text[end:]
cli_path.write_text(text)

index_path = Path("docs/architecture/INDEX.md")
text = index_path.read_text()
text = replace_once(
    text,
    '| Headless | `medusa run` | `crates/medusa-cli` | `medusa-runtime::RuntimeController` |',
    '| Headless | `medusa run` | `crates/medusa-cli` | runtime command authority; canonical journal → `medusa-protocol` frontend projection |',
    "headless architecture authority",
)
update_row = '| Update | `medusa update` | `crates/medusa-update` | Ed25519-verified prebuilt release; explicit `--channel source` developer path |\n'
text = replace_once(
    text,
    update_row,
    update_row + '\nThe phase-6 frontend migration is proceeding in production-entrypoint order. Headless CLI output now tails committed session-journal events through the versioned `medusa-protocol::frontend` projection. TUI, daemon attachment/replay, desktop, and remote voice surfaces remain explicit follow-up slices; process-local runtime events are temporary wakeups rather than user-visible lifecycle authority.\n',
    "phase-six migration status",
)
adr_row = '- Decision: [`decisions/0006-authoritative-evidence-artifacts-and-verification.md`](decisions/0006-authoritative-evidence-artifacts-and-verification.md)\n'
text = replace_once(
    text,
    adr_row,
    adr_row + '- Decision: [`decisions/0007-canonical-frontend-projection.md`](decisions/0007-canonical-frontend-projection.md)\n',
    "ADR 0007 index",
)
index_path.write_text(text)

adr_path = Path("docs/architecture/decisions/0007-canonical-frontend-projection.md")
adr_path.parent.mkdir(parents=True, exist_ok=True)
adr_path.write_text('''# ADR 0007: Canonical frontend projection and cursor authority

- **Status:** Accepted
- **Date:** 2026-08-03
- **Issue:** #652

## Context

Medusa already had a versioned `FrontendCommandEnvelope` and `FrontendEventEnvelope`, but the deterministic projection from the canonical session journal lived under the Telegram adapter. The headless CLI, TUI, and desktop each consumed their own process-local `RuntimeEvent` shape. That allowed presentation order, terminal state, and replay behavior to diverge even though the journal was authoritative.

## Decision

`medusa-protocol::frontend` owns the only journal-to-presentation projection. The projector accepts the frontend kind so delivery identities remain frontend-scoped while payload, lifecycle, redaction, and canonical cursor semantics remain identical.

A `CanonicalFrontendEventStream` in `medusa-runtime` tails committed session events and exposes versioned frontend envelopes. Its cursor is the canonical journal sequence, including skipped non-presentable events, so reconnect and replay cannot reinterpret ordering.

The phase-6 migration order is enforced in reviewable slices:

1. headless CLI consumes the canonical stream;
2. TUI consumes the same stream while retaining only view-model conversion;
3. daemon IPC owns runtime commands, attachment, and replay for process-detachable clients;
4. desktop and remote frontends attach through that daemon authority;
5. direct frontend-owned runtime projections are deleted and guarded against reintroduction.

Telegram keeps its existing `:<frontend>` event identity through a compatibility wrapper, but the wrapper contains no projection logic.

## Consequences

- A frontend cannot report completion, cancellation, verification, or integration before the corresponding committed journal event exists.
- Replayed headless output uses the same redacted event contract as remote delivery.
- Presentation cursors are stable across process restarts and do not depend on how many event kinds a renderer suppresses.
- Process-local runtime events remain temporary wakeups and compatibility inputs until the remaining phase-6 consumers migrate; they are not user-visible authority.

## Rejected alternatives

- **Keep one projector per frontend:** rejected because redaction and lifecycle interpretation drift silently.
- **Project directly from transient `RuntimeEvent`:** rejected because it cannot provide durable replay or multi-client ordering.
- **Move presentation policy into the daemon only:** rejected because protocol-level tests must remain usable by local and remote frontends without depending on daemon internals.

## Removal criteria

Phase #652 is not complete until CLI, TUI, daemon, desktop, Telegram, and voice all consume the shared command/event authority by default, direct frontend-owned terminal-state inference is deleted, and cross-client replay/cancellation/approval equivalence passes on Linux, macOS, and Windows.
''')
