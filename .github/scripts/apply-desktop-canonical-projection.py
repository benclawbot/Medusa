from __future__ import annotations

from pathlib import Path
import subprocess

EXPECTED_BLOBS = {
    "apps/medusa-desktop/src-tauri/Cargo.toml": "01523a42eedcefc2232eadbbee289fff36859bc3",
    "apps/medusa-desktop/src-tauri/src/lib.rs": "5d2be7de6811119cf99d55914bd575b771760207",
    "apps/medusa-desktop/src-tauri/src/runtime.rs": "d2214e7c1647f56e072d730bb749bf27ea9381d4",
    "apps/medusa-desktop/src-tauri/src/runtime_resume.rs": "29596dd24c7be18ef164eb62f267eedd62804f47",
    "docs/architecture/INDEX.md": "e4effb38ce8b590f0f49d1b3e4f045fb19b29185",
    "docs/architecture/decisions/0007-canonical-frontend-projection.md": "3bbd66445c147a36b04389a9c04a8ba96c17b8c5",
}


def require_blob(path: str, expected: str) -> None:
    actual = subprocess.check_output(["git", "hash-object", path], text=True).strip()
    if actual != expected:
        raise SystemExit(f"{path}: expected blob {expected}, found {actual}")


def replace_once(text: str, old: str, new: str, path: str) -> str:
    count = text.count(old)
    if count != 1:
        raise SystemExit(f"{path}: expected one replacement anchor, found {count}")
    return text.replace(old, new, 1)


for path, expected in EXPECTED_BLOBS.items():
    require_blob(path, expected)

cargo_path = Path("apps/medusa-desktop/src-tauri/Cargo.toml")
cargo = cargo_path.read_text(encoding="utf-8")
cargo = replace_once(
    cargo,
    'medusa-daemon = { path = "../../../crates/medusa-daemon" }\n',
    'medusa-daemon = { path = "../../../crates/medusa-daemon" }\nmedusa-protocol = { path = "../../../crates/medusa-protocol" }\n',
    str(cargo_path),
)
cargo_path.write_text(cargo, encoding="utf-8")

lib_path = Path("apps/medusa-desktop/src-tauri/src/lib.rs")
lib = lib_path.read_text(encoding="utf-8")
lib = replace_once(
    lib,
    'mod runtime {\n    include!("runtime.rs");\n    include!("runtime_resume.rs");',
    'mod runtime {\n    include!("runtime.rs");\n    include!("desktop_projection.rs");\n    include!("runtime_resume.rs");',
    str(lib_path),
)
lib_path.write_text(lib, encoding="utf-8")

projection_path = Path("apps/medusa-desktop/src-tauri/src/desktop_projection.rs")
projection_path.write_text(r'''use std::collections::VecDeque;

use medusa_protocol::frontend::{
    FrontendEvent, FrontendEventEnvelope, FrontendKind, PresentationActivity,
    PresentationActivityKind, PresentationLifecycle,
};
use medusa_runtime::frontend::CanonicalFrontendEventStream;

struct DesktopCanonicalPresentation {
    repo: PathBuf,
    stream: CanonicalFrontendEventStream,
    session_id: Option<String>,
    pending: VecDeque<DesktopRuntimeEvent>,
    run_active: bool,
}

impl DesktopCanonicalPresentation {
    fn new(repo: PathBuf) -> Self {
        Self {
            stream: CanonicalFrontendEventStream::new(repo.clone(), FrontendKind::Desktop),
            repo,
            session_id: None,
            pending: VecDeque::new(),
            run_active: false,
        }
    }

    fn bind_session(&mut self, session_id: &str) {
        if self.session_id.as_deref() == Some(session_id) {
            return;
        }
        self.stream = CanonicalFrontendEventStream::new(self.repo.clone(), FrontendKind::Desktop);
        self.session_id = Some(session_id.to_owned());
        self.pending.clear();
        self.run_active = false;
    }

    fn reset(&mut self) {
        self.stream = CanonicalFrontendEventStream::new(self.repo.clone(), FrontendKind::Desktop);
        self.session_id = None;
        self.pending.clear();
        self.run_active = false;
    }

    fn is_session_bound(&self) -> bool {
        self.session_id.is_some()
    }

    fn try_event(&mut self) -> Result<Option<DesktopRuntimeEvent>, String> {
        if let Some(event) = self.pending.pop_front() {
            return Ok(Some(event));
        }
        let Some(session_id) = self.session_id.clone() else {
            return Ok(None);
        };
        while let Some(envelope) = self
            .stream
            .try_event(&session_id)
            .map_err(|error| error.to_string())?
        {
            let mut events = map_frontend_event(envelope, &mut self.run_active);
            if let Some(event) = events.pop_front() {
                self.pending.extend(events);
                return Ok(Some(event));
            }
        }
        Ok(None)
    }
}

fn map_process_event(
    event: medusa_runtime::RuntimeEvent,
    session_bound: bool,
) -> Option<DesktopRuntimeEvent> {
    match &event {
        medusa_runtime::RuntimeEvent::RecoveryAvailable(_)
        | medusa_runtime::RuntimeEvent::RecoveryCompleted(_)
        | medusa_runtime::RuntimeEvent::Settings { .. }
        | medusa_runtime::RuntimeEvent::ConfigurationChanged(_)
        | medusa_runtime::RuntimeEvent::NewSession
        | medusa_runtime::RuntimeEvent::Progress { .. } => Some(event.into()),
        medusa_runtime::RuntimeEvent::Notice { title, .. }
            if !session_bound || title == "Runtime capabilities" =>
        {
            Some(event.into())
        }
        medusa_runtime::RuntimeEvent::Cancelled if !session_bound => Some(event.into()),
        medusa_runtime::RuntimeEvent::Failed(message)
            if !session_bound || is_unjournaled_publication_failure(message) =>
        {
            Some(event.into())
        }
        _ => None,
    }
}

fn is_unjournaled_publication_failure(message: &str) -> bool {
    let message = message.to_ascii_lowercase();
    message.contains("journal")
        && (message.contains("publish")
            || message.contains("persist")
            || message.contains("commit"))
}

fn map_frontend_event(
    envelope: FrontendEventEnvelope,
    run_active: &mut bool,
) -> VecDeque<DesktopRuntimeEvent> {
    let FrontendEventEnvelope {
        session_id, event, ..
    } = envelope;
    let mut events = VecDeque::new();
    match event {
        FrontendEvent::SubmissionAccepted | FrontendEvent::Started => {
            if let Some(event) = canonical_start_event(run_active) {
                events.push_back(event);
            }
        }
        FrontendEvent::SubmissionQueued { position } => {
            events.push_back(DesktopRuntimeEvent::Notice {
                title: "Follow-up queued".to_owned(),
                details: vec![format!("Queue position: {position}")],
            });
        }
        FrontendEvent::AssistantTextDelta { text }
        | FrontendEvent::AssistantInterim { text } => {
            events.push_back(DesktopRuntimeEvent::AssistantText { text });
        }
        FrontendEvent::Activity(activity) => {
            events.push_back(DesktopRuntimeEvent::Activity {
                activity: map_activity(activity),
            });
        }
        FrontendEvent::Team {
            workers,
            verification,
        } => {
            for worker in workers {
                let mut details = vec![format!("role {}", worker.role)];
                if let Some(action) = worker.current_action {
                    details.push(action);
                }
                events.push_back(DesktopRuntimeEvent::Activity {
                    activity: crate::dto::DesktopActivity {
                        id: Some(format!("team:{}", worker.worker_id)),
                        kind: activity_kind(PresentationActivityKind::Worker, worker.lifecycle),
                        title: format!("{} · {}", worker.worker_id, worker.task),
                        details,
                    },
                });
            }
            if let Some(verification) = verification {
                events.push_back(DesktopRuntimeEvent::Activity {
                    activity: crate::dto::DesktopActivity {
                        id: Some("team-verification".to_owned()),
                        kind: crate::dto::DesktopActivityKind::Verification,
                        title: "Team verification".to_owned(),
                        details: vec![verification],
                    },
                });
            }
        }
        FrontendEvent::Plan { steps, .. } => {
            events.push_back(DesktopRuntimeEvent::Plan {
                steps: steps
                    .into_iter()
                    .map(|step| crate::dto::DesktopPlanStep {
                        title: step.title,
                        status: match step.lifecycle {
                            PresentationLifecycle::Active => {
                                crate::dto::DesktopPlanStepStatus::InProgress
                            }
                            PresentationLifecycle::Succeeded => {
                                crate::dto::DesktopPlanStepStatus::Completed
                            }
                            PresentationLifecycle::Failed | PresentationLifecycle::Cancelled => {
                                crate::dto::DesktopPlanStepStatus::Failed
                            }
                            PresentationLifecycle::Waiting
                            | PresentationLifecycle::Informational => {
                                crate::dto::DesktopPlanStepStatus::Pending
                            }
                        },
                    })
                    .collect(),
            });
        }
        FrontendEvent::Question(question) => {
            events.push_back(DesktopRuntimeEvent::Question {
                prompts: vec![crate::dto::DesktopQuestionPrompt {
                    header: "Question".to_owned(),
                    question: question.prompt,
                    options: question
                        .options
                        .into_iter()
                        .map(|option| crate::dto::DesktopQuestionOption {
                            description: if option.value != option.label {
                                option.value
                            } else {
                                String::new()
                            },
                            label: option.label,
                        })
                        .collect(),
                    multi_select: false,
                }],
            });
        }
        FrontendEvent::ApprovalRequired(approval) => {
            events.push_back(DesktopRuntimeEvent::Question {
                prompts: vec![crate::dto::DesktopQuestionPrompt {
                    header: "Approval".to_owned(),
                    question: format!(
                        "{} in {}: {} (risk: {})",
                        approval.action, approval.scope, approval.reason, approval.risk
                    ),
                    options: vec![
                        crate::dto::DesktopQuestionOption {
                            label: "Approve".to_owned(),
                            description: "Allow this action once".to_owned(),
                        },
                        crate::dto::DesktopQuestionOption {
                            label: "Deny".to_owned(),
                            description: "Do not perform this action".to_owned(),
                        },
                    ],
                    multi_select: false,
                }],
            });
        }
        FrontendEvent::Usage {
            input_tokens,
            output_tokens,
            total_tokens,
            estimated_cost_microusd,
        } => {
            events.push_back(DesktopRuntimeEvent::Usage {
                input_tokens,
                output_tokens,
                cache_read_input_tokens: 0,
                cache_creation_input_tokens: 0,
                total_tokens,
                duration_ms: 0,
                tokens_per_second_milli: 0,
                estimated_cost_microusd,
                provenance: "canonical-journal".to_owned(),
            });
        }
        FrontendEvent::Progress { turn, .. } => {
            events.push_back(DesktopRuntimeEvent::Progress { turn });
        }
        FrontendEvent::SettingsChanged { .. } => {}
        FrontendEvent::Notice {
            severity,
            title,
            mut details,
        } => {
            if severity != "info" {
                details.insert(0, format!("Severity: {severity}"));
            }
            if title == "Conversation compacted" {
                events.push_back(DesktopRuntimeEvent::Compacted {
                    message: details.join(" · "),
                });
            } else {
                events.push_back(DesktopRuntimeEvent::Notice { title, details });
            }
        }
        FrontendEvent::Artifact(artifact) => {
            let mut details = vec![
                format!("Media type: {}", artifact.media_type),
                format!("Evidence: {}", artifact.evidence_ref),
            ];
            if let Some(caption) = artifact.caption {
                details.push(caption);
            }
            events.push_back(DesktopRuntimeEvent::Activity {
                activity: crate::dto::DesktopActivity {
                    id: Some(artifact.artifact_id),
                    kind: crate::dto::DesktopActivityKind::Done,
                    title: format!("Artifact available: {}", artifact.name),
                    details,
                },
            });
        }
        FrontendEvent::TurnFinished => {
            *run_active = false;
            events.push_back(DesktopRuntimeEvent::TurnFinished);
        }
        FrontendEvent::Completed { summary } => {
            *run_active = false;
            if let Some(summary) = summary {
                events.push_back(DesktopRuntimeEvent::Notice {
                    title: "Completion report".to_owned(),
                    details: vec![summary],
                });
            }
            events.push_back(DesktopRuntimeEvent::Completed { session_id });
        }
        FrontendEvent::Cancelled { reason } => {
            *run_active = false;
            if let Some(reason) = reason {
                events.push_back(DesktopRuntimeEvent::Notice {
                    title: "Cancellation reason".to_owned(),
                    details: vec![reason],
                });
            }
            events.push_back(DesktopRuntimeEvent::Cancelled);
        }
        FrontendEvent::Failed { message, recovery } => {
            *run_active = false;
            let message = if recovery.is_empty() {
                message
            } else {
                format!("{message}\nRecovery: {}", recovery.join("; "))
            };
            events.push_back(DesktopRuntimeEvent::Failed { message });
        }
    }
    events
}

fn canonical_start_event(run_active: &mut bool) -> Option<DesktopRuntimeEvent> {
    if *run_active {
        None
    } else {
        *run_active = true;
        Some(DesktopRuntimeEvent::Started)
    }
}

fn map_activity(activity: PresentationActivity) -> crate::dto::DesktopActivity {
    let mut details = activity.details;
    if !activity.affected_paths.is_empty() {
        details.push(format!("Paths: {}", activity.affected_paths.join(", ")));
    }
    if let Some(evidence) = activity.evidence_ref {
        details.push(format!("Evidence: {evidence}"));
    }
    crate::dto::DesktopActivity {
        id: Some(activity.activity_id),
        kind: activity_kind(activity.kind, activity.lifecycle),
        title: activity.title,
        details,
    }
}

fn activity_kind(
    kind: PresentationActivityKind,
    lifecycle: PresentationLifecycle,
) -> crate::dto::DesktopActivityKind {
    match lifecycle {
        PresentationLifecycle::Failed | PresentationLifecycle::Cancelled => {
            crate::dto::DesktopActivityKind::Error
        }
        PresentationLifecycle::Succeeded => match kind {
            PresentationActivityKind::Assistant => crate::dto::DesktopActivityKind::Assistant,
            PresentationActivityKind::Verification | PresentationActivityKind::Test => {
                crate::dto::DesktopActivityKind::Verification
            }
            PresentationActivityKind::Error => crate::dto::DesktopActivityKind::Error,
            _ => crate::dto::DesktopActivityKind::Done,
        },
        PresentationLifecycle::Active
        | PresentationLifecycle::Waiting
        | PresentationLifecycle::Informational => match kind {
            PresentationActivityKind::Assistant => crate::dto::DesktopActivityKind::Assistant,
            PresentationActivityKind::RepositoryRead
            | PresentationActivityKind::Edit
            | PresentationActivityKind::Command => crate::dto::DesktopActivityKind::Tool,
            PresentationActivityKind::Verification | PresentationActivityKind::Test => {
                crate::dto::DesktopActivityKind::Verification
            }
            PresentationActivityKind::Done => crate::dto::DesktopActivityKind::Done,
            PresentationActivityKind::Error => crate::dto::DesktopActivityKind::Error,
            _ => crate::dto::DesktopActivityKind::Progress,
        },
    }
}

#[cfg(test)]
mod desktop_projection_tests {
    use medusa_protocol::frontend::{
        FRONTEND_PROTOCOL_VERSION, FrontendEvent, FrontendEventEnvelope, PresentationActivity,
        PresentationActivityKind, PresentationLifecycle, PresentationPlanStep,
    };
    use time::OffsetDateTime;

    use super::*;

    fn envelope(event: FrontendEvent) -> FrontendEventEnvelope {
        FrontendEventEnvelope {
            protocol_version: FRONTEND_PROTOCOL_VERSION,
            event_id: "event-1:desktop".to_owned(),
            cursor: 1,
            session_id: "session-1".to_owned(),
            turn_id: Some("turn-1".to_owned()),
            parent_event_id: None,
            correlation_id: "correlation-1".to_owned(),
            timestamp: OffsetDateTime::UNIX_EPOCH,
            lifecycle: PresentationLifecycle::Active,
            event,
        }
    }

    #[test]
    fn canonical_start_is_emitted_once_until_terminal_state() {
        let mut run_active = false;
        assert!(matches!(
            canonical_start_event(&mut run_active),
            Some(DesktopRuntimeEvent::Started)
        ));
        assert!(canonical_start_event(&mut run_active).is_none());
        run_active = false;
        assert!(matches!(
            canonical_start_event(&mut run_active),
            Some(DesktopRuntimeEvent::Started)
        ));
    }

    #[test]
    fn session_bound_process_terminal_state_is_suppressed() {
        assert!(map_process_event(medusa_runtime::RuntimeEvent::TurnFinished, true).is_none());
        assert!(map_process_event(
            medusa_runtime::RuntimeEvent::Failed("durable failure".to_owned()),
            true,
        )
        .is_none());
        assert!(matches!(
            map_process_event(
                medusa_runtime::RuntimeEvent::Failed(
                    "journal publication failed after commit".to_owned()
                ),
                true,
            ),
            Some(DesktopRuntimeEvent::Failed { .. })
        ));
    }

    #[test]
    fn canonical_plan_and_activity_keep_the_desktop_contract() {
        let mut run_active = false;
        let plan = map_frontend_event(
            envelope(FrontendEvent::Plan {
                steps: vec![PresentationPlanStep {
                    step_id: "step-1".to_owned(),
                    title: "Wire desktop".to_owned(),
                    lifecycle: PresentationLifecycle::Active,
                }],
                current: Some("step-1".to_owned()),
            }),
            &mut run_active,
        );
        assert!(matches!(
            plan.front(),
            Some(DesktopRuntimeEvent::Plan { steps })
                if matches!(steps[0].status, crate::dto::DesktopPlanStepStatus::InProgress)
        ));

        let activity = map_frontend_event(
            envelope(FrontendEvent::Activity(PresentationActivity {
                activity_id: "verify-1".to_owned(),
                kind: PresentationActivityKind::Verification,
                lifecycle: PresentationLifecycle::Succeeded,
                title: "Desktop package".to_owned(),
                details: vec!["passed".to_owned()],
                affected_paths: vec!["apps/medusa-desktop".to_owned()],
                evidence_ref: Some("evidence-1".to_owned()),
            })),
            &mut run_active,
        );
        assert!(matches!(
            activity.front(),
            Some(DesktopRuntimeEvent::Activity { activity })
                if matches!(activity.kind, crate::dto::DesktopActivityKind::Verification)
                    && activity.id.as_deref() == Some("verify-1")
        ));
    }
}
''', encoding="utf-8")

runtime_path = Path("apps/medusa-desktop/src-tauri/src/runtime.rs")
runtime = runtime_path.read_text(encoding="utf-8")
runtime = replace_once(
    runtime,
    "struct RuntimeEntry {\n    repo: PathBuf,\n    controller: RuntimeController,\n    daemon: DesktopDaemon,\n}",
    "struct RuntimeEntry {\n    repo: PathBuf,\n    controller: RuntimeController,\n    presentation: DesktopCanonicalPresentation,\n    daemon: DesktopDaemon,\n}",
    str(runtime_path),
)
runtime = replace_once(
    runtime,
    "        let entry = Arc::new(Mutex::new(RuntimeEntry {\n            repo: repo.clone(),\n            controller,\n            daemon: DesktopDaemon {",
    "        let entry = Arc::new(Mutex::new(RuntimeEntry {\n            repo: repo.clone(),\n            controller,\n            presentation: DesktopCanonicalPresentation::new(repo.clone()),\n            daemon: DesktopDaemon {",
    str(runtime_path),
)
old_submit = '''    registry.with_entry(&runtime_id, |entry| {
        let draft = convert_prompt(&entry.repo, draft)?;
        entry
            .controller
            .submit(draft)
            .map(|disposition| match disposition {
                SubmitDisposition::Started => DesktopSubmitDisposition::Started,
                SubmitDisposition::Queued => DesktopSubmitDisposition::Queued,
            })
            .map_err(|error| error.to_string())
    })
'''
new_submit = '''    registry.with_entry(&runtime_id, |entry| {
        let draft = convert_prompt(&entry.repo, draft)?;
        let disposition = entry
            .controller
            .submit(draft)
            .map_err(|error| error.to_string())?;
        if let Some(session_id) = entry.controller.active_session_id() {
            entry.presentation.bind_session(&session_id);
        }
        Ok(match disposition {
            SubmitDisposition::Started => DesktopSubmitDisposition::Started,
            SubmitDisposition::Queued => DesktopSubmitDisposition::Queued,
        })
    })
'''
runtime = replace_once(runtime, old_submit, new_submit, str(runtime_path))
old_poll = '''    registry.with_entry(&runtime_id, |entry| {
        let mut events = Vec::new();
        let limit = max_events.unwrap_or(200).clamp(1, 500);
        if let Some(event) = entry.daemon_event() {
            events.push(event);
        }
        while events.len() < limit {
            match entry
                .controller
                .try_event()
                .map_err(|error| error.to_string())?
            {
                Some(event) => events.push(event.into()),
                None => break,
            }
        }
        Ok(events)
    })
'''
new_poll = '''    registry.with_entry(&runtime_id, |entry| {
        let mut events = Vec::new();
        let limit = max_events.unwrap_or(200).clamp(1, 500);
        if let Some(event) = entry.daemon_event() {
            events.push(event);
        }
        if let Some(session_id) = entry.controller.active_session_id() {
            entry.presentation.bind_session(&session_id);
        }
        while events.len() < limit {
            let Some(event) = entry
                .controller
                .try_event()
                .map_err(|error| error.to_string())?
            else {
                break;
            };
            if matches!(&event, medusa_runtime::RuntimeEvent::NewSession) {
                entry.presentation.reset();
            }
            if let Some(event) = map_process_event(event, entry.presentation.is_session_bound()) {
                events.push(event);
            }
        }
        while events.len() < limit {
            match entry.presentation.try_event()? {
                Some(event) => events.push(event),
                None => break,
            }
        }
        Ok(events)
    })
'''
runtime = replace_once(runtime, old_poll, new_poll, str(runtime_path))
runtime_path.write_text(runtime, encoding="utf-8")

resume_path = Path("apps/medusa-desktop/src-tauri/src/runtime_resume.rs")
resume = resume_path.read_text(encoding="utf-8")
resume = replace_once(
    resume,
    "        let entry = Arc::new(Mutex::new(RuntimeEntry {\n            repo,\n            controller,\n            daemon: DesktopDaemon {",
    "        let mut presentation = DesktopCanonicalPresentation::new(repo.clone());\n        presentation.bind_session(session_id);\n        let entry = Arc::new(Mutex::new(RuntimeEntry {\n            repo,\n            controller,\n            presentation,\n            daemon: DesktopDaemon {",
    str(resume_path),
)
resume_path.write_text(resume, encoding="utf-8")

index_path = Path("docs/architecture/INDEX.md")
index = index_path.read_text(encoding="utf-8")
index = replace_once(
    index,
    "| Desktop | `apps/medusa-desktop` | React/Tauri application | `medusa-runtime::RuntimeController` |",
    "| Desktop | `apps/medusa-desktop` | React/Tauri application | runtime command compatibility; canonical journal → `medusa-protocol` desktop projection |",
    str(index_path),
)
index = replace_once(
    index,
    "The TUI temporarily retains local settings, startup recovery, turn-counter, and reset hints; desktop and remote voice surfaces remain follow-up slices.",
    "The TUI and desktop temporarily retain local settings, startup recovery, turn-counter, reset hints, and process-local command compatibility; desktop daemon-command migration and remote voice remain follow-up slices.",
    str(index_path),
)
index_path.write_text(index, encoding="utf-8")

adr_path = Path("docs/architecture/decisions/0007-canonical-frontend-projection.md")
adr = adr_path.read_text(encoding="utf-8")
adr = replace_once(
    adr,
    "The TUI keeps process-local settings, startup recovery, turn-counter, and reset hints only as bounded compatibility inputs while desktop migration is completed.",
    "The desktop now consumes `FrontendKind::Desktop` envelopes for durable transcript, plan, activity, question, usage, cancellation, failure, and completion state. TUI and desktop keep process-local settings, startup recovery, turn-counter, reset hints, and desktop command execution only as bounded compatibility inputs while desktop commands and attachments move to daemon protocol v2.",
    str(adr_path),
)
adr_path.write_text(adr, encoding="utf-8")
