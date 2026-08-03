from __future__ import annotations

from pathlib import Path
import subprocess

EXPECTED_BLOBS = {
    "crates/medusa-tui/Cargo.toml": "6efb1730f8ff282a4ee1100c60e2d81c5faa843d",
    "crates/medusa-tui/src/runtime.rs": "a965da5f336548ecc57454edb91d204acffd05fa",
    "docs/architecture/INDEX.md": "660a93c41fdd9c0fb7d2e81253e4454232f4c7e8",
    "docs/architecture/decisions/0007-canonical-frontend-projection.md": "883cd54b34cfc3dabccd14b83a7d361cff8cc645",
}


def require_blob(path: str, expected: str) -> None:
    actual = subprocess.check_output(["git", "hash-object", path], text=True).strip()
    if actual != expected:
        raise SystemExit(f"{path}: expected blob {expected}, found {actual}")


def replace_once(text: str, old: str, new: str, path: str) -> str:
    if text.count(old) != 1:
        raise SystemExit(f"{path}: expected one occurrence of replacement anchor, found {text.count(old)}")
    return text.replace(old, new, 1)


for path, expected in EXPECTED_BLOBS.items():
    require_blob(path, expected)

cargo_path = Path("crates/medusa-tui/Cargo.toml")
cargo = cargo_path.read_text(encoding="utf-8")
cargo = replace_once(
    cargo,
    'medusa-daemon = { path = "../medusa-daemon" }\n',
    'medusa-daemon = { path = "../medusa-daemon" }\nmedusa-protocol = { path = "../medusa-protocol" }\n',
    str(cargo_path),
)
cargo_path.write_text(cargo, encoding="utf-8")

runtime_path = Path("crates/medusa-tui/src/runtime.rs")
runtime = runtime_path.read_text(encoding="utf-8")
runtime = replace_once(
    runtime,
    "use std::{\n    path::PathBuf,",
    "use std::{\n    collections::VecDeque,\n    path::PathBuf,",
    str(runtime_path),
)
runtime = replace_once(
    runtime,
    "use crate::commands::{ModelConfiguration, SlashCommand};\n",
    "use crate::commands::{ModelConfiguration, SlashCommand};\n"
    "use medusa_protocol::frontend::{\n"
    "    FrontendEvent, FrontendEventEnvelope, FrontendKind, PresentationActivity,\n"
    "    PresentationActivityKind, PresentationLifecycle,\n"
    "};\n"
    "use medusa_runtime::frontend::CanonicalFrontendEventStream;\n",
    str(runtime_path),
)
runtime = replace_once(
    runtime,
    "pub struct RuntimeController {\n"
    "    inner: Arc<Mutex<medusa_runtime::RuntimeController>>,\n"
    "    deferred_events: Receiver<RuntimeEvent>,\n"
    "    deferred_event_sender: Sender<RuntimeEvent>,\n"
    "    submission_in_flight: Arc<AtomicBool>,\n"
    "}\n",
    "pub struct RuntimeController {\n"
    "    inner: Arc<Mutex<medusa_runtime::RuntimeController>>,\n"
    "    canonical: Mutex<CanonicalPresentation>,\n"
    "    active_session_id: Mutex<Option<String>>,\n"
    "    deferred_events: Receiver<RuntimeEvent>,\n"
    "    deferred_event_sender: Sender<RuntimeEvent>,\n"
    "    submission_in_flight: Arc<AtomicBool>,\n"
    "}\n\n"
    "struct CanonicalPresentation {\n"
    "    repo: PathBuf,\n"
    "    stream: CanonicalFrontendEventStream,\n"
    "    session_id: Option<String>,\n"
    "    pending: VecDeque<RuntimeEvent>,\n"
    "    run_active: bool,\n"
    "}\n\n"
    "impl CanonicalPresentation {\n"
    "    fn new(repo: PathBuf) -> Self {\n"
    "        Self {\n"
    "            stream: CanonicalFrontendEventStream::new(repo.clone(), FrontendKind::Tui),\n"
    "            repo,\n"
    "            session_id: None,\n"
    "            pending: VecDeque::new(),\n"
    "            run_active: false,\n"
    "        }\n"
    "    }\n\n"
    "    fn reset(&mut self) {\n"
    "        self.stream = CanonicalFrontendEventStream::new(self.repo.clone(), FrontendKind::Tui);\n"
    "        self.session_id = None;\n"
    "        self.pending.clear();\n"
    "        self.run_active = false;\n"
    "    }\n\n"
    "    fn try_event(&mut self, session_id: &str) -> Result<Option<RuntimeEvent>, RuntimeError> {\n"
    "        if self.session_id.as_deref() != Some(session_id) {\n"
    "            self.session_id = Some(session_id.to_owned());\n"
    "            self.pending.clear();\n"
    "            self.run_active = false;\n"
    "        }\n"
    "        if let Some(event) = self.pending.pop_front() {\n"
    "            return Ok(Some(event));\n"
    "        }\n"
    "        while let Some(envelope) = self.stream.try_event(session_id)? {\n"
    "            let mut events = map_frontend_event(envelope, &mut self.run_active);\n"
    "            if let Some(event) = events.pop_front() {\n"
    "                self.pending.extend(events);\n"
    "                return Ok(Some(event));\n"
    "            }\n"
    "        }\n"
    "        Ok(None)\n"
    "    }\n"
    "}\n",
    str(runtime_path),
)
runtime = replace_once(
    runtime,
    "    pub fn start(repo: PathBuf) -> Self {\n"
    "        Self::from_inner(medusa_runtime::RuntimeController::start(repo))\n"
    "    }\n\n"
    "    pub fn start_resumed(repo: PathBuf, session_id: &str) -> Result<Self, RuntimeError> {\n"
    "        medusa_runtime::RuntimeController::start_resumed(repo, session_id).map(Self::from_inner)\n"
    "    }\n\n"
    "    pub fn start_continue_latest(repo: PathBuf) -> Result<Self, RuntimeError> {\n"
    "        medusa_runtime::RuntimeController::start_continue_latest(repo).map(Self::from_inner)\n"
    "    }\n\n"
    "    fn from_inner(inner: medusa_runtime::RuntimeController) -> Self {\n"
    "        let (deferred_event_sender, deferred_events) = mpsc::channel();\n"
    "        Self {\n"
    "            inner: Arc::new(Mutex::new(inner)),\n"
    "            deferred_events,\n"
    "            deferred_event_sender,\n"
    "            submission_in_flight: Arc::new(AtomicBool::new(false)),\n"
    "        }\n"
    "    }\n",
    "    pub fn start(repo: PathBuf) -> Self {\n"
    "        let inner = medusa_runtime::RuntimeController::start(repo.clone());\n"
    "        Self::from_inner(repo, inner)\n"
    "    }\n\n"
    "    pub fn start_resumed(repo: PathBuf, session_id: &str) -> Result<Self, RuntimeError> {\n"
    "        let inner = medusa_runtime::RuntimeController::start_resumed(repo.clone(), session_id)?;\n"
    "        Ok(Self::from_inner(repo, inner))\n"
    "    }\n\n"
    "    pub fn start_continue_latest(repo: PathBuf) -> Result<Self, RuntimeError> {\n"
    "        let inner = medusa_runtime::RuntimeController::start_continue_latest(repo.clone())?;\n"
    "        Ok(Self::from_inner(repo, inner))\n"
    "    }\n\n"
    "    fn from_inner(repo: PathBuf, inner: medusa_runtime::RuntimeController) -> Self {\n"
    "        let (deferred_event_sender, deferred_events) = mpsc::channel();\n"
    "        Self {\n"
    "            inner: Arc::new(Mutex::new(inner)),\n"
    "            canonical: Mutex::new(CanonicalPresentation::new(repo)),\n"
    "            active_session_id: Mutex::new(None),\n"
    "            deferred_events,\n"
    "            deferred_event_sender,\n"
    "            submission_in_flight: Arc::new(AtomicBool::new(false)),\n"
    "        }\n"
    "    }\n",
    str(runtime_path),
)
runtime = replace_once(
    runtime,
    "    pub fn try_event(&self) -> Result<Option<RuntimeEvent>, RuntimeError> {\n"
    "        match self.deferred_events.try_recv() {\n"
    "            Ok(event) => return Ok(Some(event)),\n"
    "            Err(TryRecvError::Disconnected | TryRecvError::Empty) => {}\n"
    "        }\n"
    "        match self.inner.try_lock() {\n"
    "            Ok(inner) => inner.try_event().map(|event| event.map(map_event)),\n"
    "            Err(TryLockError::Poisoned(poisoned)) => poisoned\n"
    "                .into_inner()\n"
    "                .try_event()\n"
    "                .map(|event| event.map(map_event)),\n"
    "            Err(TryLockError::WouldBlock) => Ok(None),\n"
    "        }\n"
    "    }\n",
    "    pub fn try_event(&self) -> Result<Option<RuntimeEvent>, RuntimeError> {\n"
    "        match self.deferred_events.try_recv() {\n"
    "            Ok(event) => return Ok(Some(event)),\n"
    "            Err(TryRecvError::Disconnected | TryRecvError::Empty) => {}\n"
    "        }\n\n"
    "        let (transient, observed_session_id) = match self.inner.try_lock() {\n"
    "            Ok(inner) => (inner.try_event()?, inner.active_session_id()),\n"
    "            Err(TryLockError::Poisoned(poisoned)) => {\n"
    "                let inner = poisoned.into_inner();\n"
    "                (inner.try_event()?, inner.active_session_id())\n"
    "            }\n"
    "            Err(TryLockError::WouldBlock) => (None, None),\n"
    "        };\n\n"
    "        let reset = matches!(\n"
    "            transient.as_ref(),\n"
    "            Some(medusa_runtime::RuntimeEvent::NewSession)\n"
    "        );\n"
    "        let session_id = if reset {\n"
    "            *lock(&self.active_session_id) = None;\n"
    "            lock(&self.canonical).reset();\n"
    "            None\n"
    "        } else if let Some(session_id) = observed_session_id {\n"
    "            *lock(&self.active_session_id) = Some(session_id.clone());\n"
    "            Some(session_id)\n"
    "        } else {\n"
    "            lock(&self.active_session_id).clone()\n"
    "        };\n\n"
    "        if let Some(event) = transient\n"
    "            .and_then(|event| map_process_event(event, session_id.is_some()))\n"
    "        {\n"
    "            return Ok(Some(event));\n"
    "        }\n"
    "        let Some(session_id) = session_id else {\n"
    "            return Ok(None);\n"
    "        };\n"
    "        lock(&self.canonical).try_event(&session_id)\n"
    "    }\n",
    str(runtime_path),
)
runtime = replace_once(
    runtime,
    "                Ok(SubmitDisposition::Queued) => {\n"
    "                    let _ = events.send(RuntimeEvent::Notice {\n"
    "                        title: \"Follow-up queued\".to_owned(),\n"
    "                        details: vec![\n"
    "                            \"The prompt will run after the active agent turn.\".to_owned(),\n"
    "                        ],\n"
    "                    });\n"
    "                }\n",
    "                Ok(SubmitDisposition::Queued) => {}\n",
    str(runtime_path),
)
start = runtime.index("fn map_event(event: medusa_runtime::RuntimeEvent) -> RuntimeEvent {")
end = runtime.index("#[cfg(test)]\nmod tests {", start)
helpers = r'''fn map_process_event(
    event: medusa_runtime::RuntimeEvent,
    session_bound: bool,
) -> Option<RuntimeEvent> {
    match event {
        medusa_runtime::RuntimeEvent::RecoveryAvailable(view) => Some(RuntimeEvent::Notice {
            title: "Recovery available".to_owned(),
            details: recovery_details(&view),
        }),
        medusa_runtime::RuntimeEvent::Settings {
            model,
            effort,
            plan_mode,
            credential_configured,
            context_window_tokens,
            auto_compact_percent,
        } => Some(RuntimeEvent::Settings {
            model,
            effort,
            plan_mode,
            credential_configured,
            context_window_tokens,
            auto_compact_percent,
        }),
        medusa_runtime::RuntimeEvent::ConfigurationChanged(change) => Some(RuntimeEvent::Notice {
            title: format!("Configuration revision {} applied", change.revision),
            details: vec![
                format!("Profile: {}", change.active_profile),
                format!("Changed: {}", change.changed_keys.join(", ")),
                format!("Origin: {:?}", change.origin),
                format!("Apply timing: {:?}", change.apply_timing),
            ],
        }),
        medusa_runtime::RuntimeEvent::Notice { title, details }
            if title == "Runtime capabilities" =>
        {
            Some(RuntimeEvent::Activity(RuntimeActivity {
                id: Some("runtime-capabilities".to_owned()),
                kind: RuntimeActivityKind::Done,
                title,
                details,
            }))
        }
        medusa_runtime::RuntimeEvent::Notice { title, details } => {
            Some(RuntimeEvent::Notice { title, details })
        }
        medusa_runtime::RuntimeEvent::NewSession => Some(RuntimeEvent::NewSession),
        medusa_runtime::RuntimeEvent::Progress { turn } => Some(RuntimeEvent::Progress { turn }),
        medusa_runtime::RuntimeEvent::Cancelled if !session_bound => Some(RuntimeEvent::Cancelled),
        medusa_runtime::RuntimeEvent::Failed(error) if !session_bound => {
            Some(RuntimeEvent::Failed(error))
        }
        medusa_runtime::RuntimeEvent::RecoveryCompleted(_)
        | medusa_runtime::RuntimeEvent::Started
        | medusa_runtime::RuntimeEvent::AssistantText(_)
        | medusa_runtime::RuntimeEvent::Activity(_)
        | medusa_runtime::RuntimeEvent::Team(_)
        | medusa_runtime::RuntimeEvent::Plan(_)
        | medusa_runtime::RuntimeEvent::Question(_)
        | medusa_runtime::RuntimeEvent::Usage { .. }
        | medusa_runtime::RuntimeEvent::Compacted { .. }
        | medusa_runtime::RuntimeEvent::Completed { .. }
        | medusa_runtime::RuntimeEvent::TurnFinished
        | medusa_runtime::RuntimeEvent::Cancelled
        | medusa_runtime::RuntimeEvent::Failed(_) => None,
    }
}

fn map_frontend_event(
    envelope: FrontendEventEnvelope,
    run_active: &mut bool,
) -> VecDeque<RuntimeEvent> {
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
            events.push_back(RuntimeEvent::Notice {
                title: "Follow-up queued".to_owned(),
                details: vec![format!("Queue position: {position}")],
            });
        }
        FrontendEvent::AssistantTextDelta { text }
        | FrontendEvent::AssistantInterim { text } => {
            events.push_back(RuntimeEvent::AssistantText(text));
        }
        FrontendEvent::Activity(activity) => {
            events.push_back(RuntimeEvent::Activity(map_presentation_activity(activity)));
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
                events.push_back(RuntimeEvent::Activity(RuntimeActivity {
                    id: Some(format!("team:{}", worker.worker_id)),
                    kind: runtime_activity_kind(
                        PresentationActivityKind::Worker,
                        worker.lifecycle,
                    ),
                    title: format!("{} · {}", worker.worker_id, worker.task),
                    details,
                }));
            }
            if let Some(verification) = verification {
                events.push_back(RuntimeEvent::Activity(RuntimeActivity {
                    id: Some("team-verification".to_owned()),
                    kind: RuntimeActivityKind::Verification,
                    title: "Team verification".to_owned(),
                    details: vec![verification],
                }));
            }
        }
        FrontendEvent::Plan { steps, .. } => {
            events.push_back(RuntimeEvent::Plan(TranscriptPlan {
                steps: steps
                    .into_iter()
                    .map(|step| TranscriptPlanStep {
                        title: step.title,
                        state: match step.lifecycle {
                            PresentationLifecycle::Active => TranscriptPlanStepState::Active,
                            PresentationLifecycle::Succeeded => {
                                TranscriptPlanStepState::Completed
                            }
                            PresentationLifecycle::Failed
                            | PresentationLifecycle::Cancelled => TranscriptPlanStepState::Failed,
                            PresentationLifecycle::Waiting
                            | PresentationLifecycle::Informational => {
                                TranscriptPlanStepState::Pending
                            }
                        },
                    })
                    .collect(),
            }));
        }
        FrontendEvent::Question(question) => {
            events.push_back(RuntimeEvent::Question(RuntimeQuestion {
                questions: vec![QuestionPrompt {
                    header: "Question".to_owned(),
                    question: question.prompt,
                    options: question
                        .options
                        .into_iter()
                        .map(|option| QuestionOption {
                            description: (option.value != option.label)
                                .then_some(option.value)
                                .unwrap_or_default(),
                            label: option.label,
                        })
                        .collect(),
                    multi_select: false,
                }],
            }));
        }
        FrontendEvent::ApprovalRequired(approval) => {
            events.push_back(RuntimeEvent::Question(RuntimeQuestion {
                questions: vec![QuestionPrompt {
                    header: "Approval".to_owned(),
                    question: format!(
                        "{} in {}: {} (risk: {})",
                        approval.action, approval.scope, approval.reason, approval.risk
                    ),
                    options: vec![
                        QuestionOption {
                            label: "Approve".to_owned(),
                            description: "Allow this action once".to_owned(),
                        },
                        QuestionOption {
                            label: "Deny".to_owned(),
                            description: "Do not perform this action".to_owned(),
                        },
                    ],
                    multi_select: false,
                }],
            }));
        }
        FrontendEvent::Usage {
            input_tokens,
            output_tokens,
            total_tokens,
            estimated_cost_microusd,
        } => {
            events.push_back(RuntimeEvent::Usage {
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
            events.push_back(RuntimeEvent::Progress { turn });
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
                events.push_back(RuntimeEvent::Compacted {
                    message: details.join(" · "),
                });
            } else {
                events.push_back(RuntimeEvent::Notice { title, details });
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
            events.push_back(RuntimeEvent::Activity(RuntimeActivity {
                id: Some(artifact.artifact_id),
                kind: RuntimeActivityKind::Done,
                title: format!("Artifact available: {}", artifact.name),
                details,
            }));
        }
        FrontendEvent::TurnFinished => {
            *run_active = false;
            events.push_back(RuntimeEvent::TurnFinished);
        }
        FrontendEvent::Completed { summary } => {
            *run_active = false;
            if let Some(summary) = summary {
                events.push_back(RuntimeEvent::Notice {
                    title: "Completion report".to_owned(),
                    details: vec![summary],
                });
            }
            events.push_back(RuntimeEvent::Completed { session_id });
        }
        FrontendEvent::Cancelled { reason } => {
            *run_active = false;
            if let Some(reason) = reason {
                events.push_back(RuntimeEvent::Notice {
                    title: "Cancellation reason".to_owned(),
                    details: vec![reason],
                });
            }
            events.push_back(RuntimeEvent::Cancelled);
        }
        FrontendEvent::Failed { message, recovery } => {
            *run_active = false;
            let message = if recovery.is_empty() {
                message
            } else {
                format!("{message}\nRecovery: {}", recovery.join("; "))
            };
            events.push_back(RuntimeEvent::Failed(message));
        }
    }
    events
}

fn canonical_start_event(run_active: &mut bool) -> Option<RuntimeEvent> {
    if *run_active {
        None
    } else {
        *run_active = true;
        Some(RuntimeEvent::Started)
    }
}

fn map_presentation_activity(activity: PresentationActivity) -> RuntimeActivity {
    let mut details = activity.details;
    if !activity.affected_paths.is_empty() {
        details.push(format!("Paths: {}", activity.affected_paths.join(", ")));
    }
    if let Some(evidence) = activity.evidence_ref {
        details.push(format!("Evidence: {evidence}"));
    }
    RuntimeActivity {
        id: Some(activity.activity_id),
        kind: runtime_activity_kind(activity.kind, activity.lifecycle),
        title: activity.title,
        details,
    }
}

fn runtime_activity_kind(
    kind: PresentationActivityKind,
    lifecycle: PresentationLifecycle,
) -> RuntimeActivityKind {
    match lifecycle {
        PresentationLifecycle::Failed | PresentationLifecycle::Cancelled => {
            RuntimeActivityKind::Error
        }
        PresentationLifecycle::Succeeded => match kind {
            PresentationActivityKind::Assistant => RuntimeActivityKind::Assistant,
            PresentationActivityKind::Verification | PresentationActivityKind::Test => {
                RuntimeActivityKind::Verification
            }
            PresentationActivityKind::Error => RuntimeActivityKind::Error,
            _ => RuntimeActivityKind::Done,
        },
        PresentationLifecycle::Active
        | PresentationLifecycle::Waiting
        | PresentationLifecycle::Informational => match kind {
            PresentationActivityKind::Assistant => RuntimeActivityKind::Assistant,
            PresentationActivityKind::RepositoryRead
            | PresentationActivityKind::Edit
            | PresentationActivityKind::Command => RuntimeActivityKind::Tool,
            PresentationActivityKind::Verification | PresentationActivityKind::Test => {
                RuntimeActivityKind::Verification
            }
            PresentationActivityKind::Done => RuntimeActivityKind::Done,
            PresentationActivityKind::Error => RuntimeActivityKind::Error,
            _ => RuntimeActivityKind::Progress,
        },
    }
}

fn recovery_details(view: &medusa_recovery_coordinator::RecoveryView) -> Vec<String> {
    let mut details = vec![
        format!("Session: {}", view.session_id),
        format!("Last durable step: {}", view.last_durable_step),
        format!("Health: {:?}", view.health),
        format!("Checkpoints: {}", view.checkpoints.len()),
    ];
    if let Some(operation) = &view.interrupted_operation {
        details.push(format!("Interrupted: {operation}"));
    }
    details.extend(view.warnings.iter().cloned());
    details
}

'''
runtime = runtime[:start] + helpers + runtime[end:]
runtime = replace_once(
    runtime,
    "    #[test]\n    fn submission_error_is_forwarded_without_blocking_dispatch() {",
    "    #[test]\n"
    "    fn canonical_start_is_emitted_once_until_terminal_state() {\n"
    "        let mut run_active = false;\n"
    "        assert!(matches!(\n"
    "            canonical_start_event(&mut run_active),\n"
    "            Some(RuntimeEvent::Started)\n"
    "        ));\n"
    "        assert!(canonical_start_event(&mut run_active).is_none());\n"
    "        run_active = false;\n"
    "        assert!(matches!(\n"
    "            canonical_start_event(&mut run_active),\n"
    "            Some(RuntimeEvent::Started)\n"
    "        ));\n"
    "    }\n\n"
    "    #[test]\n"
    "    fn session_bound_process_terminal_state_is_suppressed() {\n"
    "        assert!(\n"
    "            map_process_event(medusa_runtime::RuntimeEvent::TurnFinished, true).is_none()\n"
    "        );\n"
    "        assert!(map_process_event(\n"
    "            medusa_runtime::RuntimeEvent::Failed(\"durable failure\".to_owned()),\n"
    "            true,\n"
    "        )\n"
    "        .is_none());\n"
    "    }\n\n"
    "    #[test]\n"
    "    fn pre_session_failure_remains_visible() {\n"
    "        assert!(matches!(\n"
    "            map_process_event(\n"
    "                medusa_runtime::RuntimeEvent::Failed(\"startup failed\".to_owned()),\n"
    "                false,\n"
    "            ),\n"
    "            Some(RuntimeEvent::Failed(error)) if error == \"startup failed\"\n"
    "        ));\n"
    "    }\n\n"
    "    #[test]\n"
    "    fn process_turn_projection_remains_a_compatibility_hint() {\n"
    "        assert!(matches!(\n"
    "            map_process_event(medusa_runtime::RuntimeEvent::Progress { turn: 7 }, true),\n"
    "            Some(RuntimeEvent::Progress { turn: 7 })\n"
    "        ));\n"
    "    }\n\n"
    "    #[test]\n    fn submission_error_is_forwarded_without_blocking_dispatch() {",
    str(runtime_path),
)
runtime_path.write_text(runtime, encoding="utf-8")

index_path = Path("docs/architecture/INDEX.md")
index = index_path.read_text(encoding="utf-8")
index = replace_once(
    index,
    "| Interactive terminal | `medusa` | `crates/medusa-tui` | `medusa-runtime::RuntimeController` |",
    "| Interactive terminal | `medusa` | `crates/medusa-tui` | runtime command authority; canonical journal → `medusa-protocol` TUI projection |",
    str(index_path),
)
index = replace_once(
    index,
    "The phase-6 frontend migration is proceeding in production-entrypoint order. Headless CLI output now tails committed session-journal events through the versioned `medusa-protocol::frontend` projection. TUI, daemon attachment/replay, desktop, and remote voice surfaces remain explicit follow-up slices; process-local runtime events are temporary wakeups rather than user-visible lifecycle authority.",
    "The phase-6 frontend migration is proceeding in production-entrypoint order. Headless CLI and interactive TUI transcript/lifecycle output now tail committed session-journal events through the versioned `medusa-protocol::frontend` projection. The TUI temporarily retains process-local settings, startup recovery, turn-counter, and explicit reset hints; daemon attachment/replay, desktop, and remote voice surfaces remain follow-up slices.",
    str(index_path),
)
index_path.write_text(index, encoding="utf-8")

adr_path = Path("docs/architecture/decisions/0007-canonical-frontend-projection.md")
adr = adr_path.read_text(encoding="utf-8")
adr = replace_once(
    adr,
    "Telegram keeps its existing `:<frontend>` event identity through a compatibility wrapper, but the wrapper contains no projection logic.\n\n## Consequences",
    "Telegram keeps its existing `:<frontend>` event identity through a compatibility wrapper, but the wrapper contains no projection logic.\n\n## Migration status\n\nThe headless CLI and interactive TUI now consume the canonical stream for durable transcript, plan, question, activity, usage, cancellation, failure, and completion state. The TUI keeps process-local settings, startup recovery, turn-counter, and explicit reset hints only as bounded compatibility inputs while daemon attachment/replay is built.\n\n## Consequences",
    str(adr_path),
)
adr_path.write_text(adr, encoding="utf-8")
