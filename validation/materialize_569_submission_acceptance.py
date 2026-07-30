from pathlib import Path

runtime_path = Path("crates/medusa-runtime/src/lib.rs")
source = runtime_path.read_text()

replacements = [
    (
        "    Submit(PromptDraft),\n",
        "    Submit {\n        draft: PromptDraft,\n        accepted: Sender<()>,\n    },\n",
    ),
    (
        """        if submission.busy {
            let command_id = next_followup_command_id();
            let mut queued = QueuedFollowup {
                command_id: command_id.clone(),
                draft,
                durably_recorded: false,
            };
            if let Some(session_id) = submission.active_session_id.as_deref() {
                record_controller_event(
                    &self.repo,
                    session_id,
                    Actor::User,
                    EventPayload::UserFollowupQueued {
                        command_id,
                        prompt: serde_json::to_value(&queued.draft).map_err(RuntimeError::agent)?,
                    },
                )?;
                queued.durably_recorded = true;
            }
            submission.followups.push_back(queued);
            return Ok(SubmitDisposition::Queued);
        }
        submission.busy = true;
        self.cancel.store(false, Ordering::SeqCst);
        if self.commands.send(RuntimeCommand::Submit(draft)).is_err() {
            submission.busy = false;
            return Err(RuntimeError::WorkerStopped);
        }
        Ok(SubmitDisposition::Started)
""",
        """        if submission.busy {
            let session_id = submission
                .active_session_id
                .clone()
                .ok_or(RuntimeError::Busy)?;
            let command_id = next_followup_command_id();
            let queued = QueuedFollowup {
                command_id: command_id.clone(),
                draft,
                durably_recorded: true,
            };
            record_controller_event(
                &self.repo,
                &session_id,
                Actor::User,
                EventPayload::UserFollowupQueued {
                    command_id,
                    prompt: serde_json::to_value(&queued.draft).map_err(RuntimeError::agent)?,
                },
            )?;
            submission.followups.push_back(queued);
            return Ok(SubmitDisposition::Queued);
        }
        submission.busy = true;
        drop(submission);
        self.cancel.store(false, Ordering::SeqCst);
        let (accepted_tx, accepted_rx) = mpsc::channel();
        if self
            .commands
            .send(RuntimeCommand::Submit {
                draft,
                accepted: accepted_tx,
            })
            .is_err()
        {
            mark_idle(&self.submission, true);
            return Err(RuntimeError::WorkerStopped);
        }
        if accepted_rx.recv().is_err() {
            mark_idle(&self.submission, true);
            return Err(RuntimeError::agent(
                "runtime prompt ended before a durable session accepted the submission",
            ));
        }
        Ok(SubmitDisposition::Started)
""",
    ),
    (
        """            RuntimeCommand::Submit(draft) => {
                let _ = events.send(RuntimeEvent::Started);
                let outcome = run_prompt(&mut state, draft, &events, &cancel, &submission);
""",
        """            RuntimeCommand::Submit { draft, accepted } => {
                let _ = events.send(RuntimeEvent::Started);
                let outcome = run_prompt(
                    &mut state,
                    draft,
                    &events,
                    &cancel,
                    &submission,
                    Some(accepted),
                );
""",
    ),
    (
        """fn run_prompt(
    state: &mut RuntimeState,
    draft: PromptDraft,
    events: &Sender<RuntimeEvent>,
    cancel: &Arc<AtomicBool>,
    submission: &Arc<Mutex<SubmissionState>>,
) -> Result<RuntimeEvent, RuntimeError> {
""",
        """fn run_prompt(
    state: &mut RuntimeState,
    draft: PromptDraft,
    events: &Sender<RuntimeEvent>,
    cancel: &Arc<AtomicBool>,
    submission: &Arc<Mutex<SubmissionState>>,
    accepted: Option<Sender<()>>,
) -> Result<RuntimeEvent, RuntimeError> {
""",
    ),
    (
        """    lock_submission(submission).active_session_id = Some(session.id.to_string());
    state.session = Some(session);
""",
        """    lock_submission(submission).active_session_id = Some(session.id.to_string());
    state.session = Some(session);
    if let Some(accepted) = accepted {
        let _ = accepted.send(());
    }
""",
    ),
    (
        """                    events,
                    cancel,
                    submission,
                )
""",
        """                    events,
                    cancel,
                    submission,
                    None,
                )
""",
    ),
    (
        """                        events,
                        cancel,
                        submission,
                    )
""",
        """                        events,
                        cancel,
                        submission,
                        None,
                    )
""",
    ),
]

if "accepted: Sender<()>" not in source:
    for old, new in replacements:
        count = source.count(old)
        if count != 1:
            raise SystemExit(
                f"expected exactly one replacement target, found {count}: {old[:80]!r}"
            )
        source = source.replace(old, new, 1)
    runtime_path.write_text(source)

tests_path = Path("crates/medusa-runtime/src/tests.rs")
tests = tests_path.read_text()
marker = "fn initial_submit_waits_for_session_acceptance_before_returning()"
if marker not in tests:
    tests += r'''

#[test]
fn initial_submit_waits_for_session_acceptance_before_returning() {
    let directory = tempdir().expect("temporary directory");
    let submission = std::sync::Arc::new(std::sync::Mutex::new(SubmissionState::default()));
    let (command_tx, command_rx) = mpsc::channel();
    let (_frontend_tx, frontend_rx) = mpsc::channel();
    let (event_sender, _runtime_event_rx) = mpsc::channel();
    let runtime = RuntimeController {
        commands: command_tx,
        events: frontend_rx,
        cancel: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        submission: std::sync::Arc::clone(&submission),
        event_sender,
        team_control: TeamControlPlane::default(),
        repo: directory.path().to_path_buf(),
    };
    let worker_submission = std::sync::Arc::clone(&submission);
    let worker = thread::spawn(move || {
        let RuntimeCommand::Submit { draft, accepted } =
            command_rx.recv().expect("submission command")
        else {
            panic!("expected submission command");
        };
        assert_eq!(draft.text, "start a durable session");
        let mut state = worker_submission.lock().expect("submission state");
        assert!(state.busy);
        state.active_session_id = Some("session-accepted".to_owned());
        drop(state);
        accepted.send(()).expect("accept submission");
    });

    assert_eq!(
        runtime
            .submit(PromptDraft {
                text: "start a durable session".to_owned(),
                ..PromptDraft::default()
            })
            .expect("accepted submission"),
        SubmitDisposition::Started
    );
    worker.join().expect("worker joins");
    assert_eq!(
        submission
            .lock()
            .expect("submission state")
            .active_session_id
            .as_deref(),
        Some("session-accepted")
    );
}

#[test]
fn followup_fails_closed_until_a_durable_session_identity_exists() {
    let directory = tempdir().expect("temporary directory");
    let submission = std::sync::Arc::new(std::sync::Mutex::new(SubmissionState {
        busy: true,
        active_session_id: None,
        ..SubmissionState::default()
    }));
    let (command_tx, command_rx) = mpsc::channel();
    let (_frontend_tx, frontend_rx) = mpsc::channel();
    let (event_sender, _runtime_event_rx) = mpsc::channel();
    let runtime = RuntimeController {
        commands: command_tx,
        events: frontend_rx,
        cancel: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        submission: std::sync::Arc::clone(&submission),
        event_sender,
        team_control: TeamControlPlane::default(),
        repo: directory.path().to_path_buf(),
    };

    assert!(matches!(
        runtime.submit(PromptDraft {
            text: "do not acknowledge this before durability".to_owned(),
            ..PromptDraft::default()
        }),
        Err(RuntimeError::Busy)
    ));
    assert!(command_rx.try_recv().is_err());
    assert!(
        submission
            .lock()
            .expect("submission state")
            .followups
            .is_empty()
    );
}
'''
    tests_path.write_text(tests)
