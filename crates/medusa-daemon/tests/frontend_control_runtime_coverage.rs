use medusa_agent::AgentEngine;
use medusa_config::Config;
use medusa_core::MedusaResult;
use medusa_daemon::{FrontendControlError, FrontendControlPlane, FrontendControlResult};
use medusa_protocol::frontend::{
    ApprovalDecision, AttachmentMode, FRONTEND_PROTOCOL_VERSION, FrontendCommand,
    FrontendCommandEnvelope, FrontendKind,
};
use medusa_provider::{ModelProvider, ModelRequest, ModelResponse};
use tempfile::tempdir;
use time::macros::datetime;

struct UnusedProvider;

impl ModelProvider for UnusedProvider {
    fn complete(&self, _: &ModelRequest) -> MedusaResult<ModelResponse> {
        unreachable!("session creation does not call the provider")
    }
}

fn envelope(
    sequence: u32,
    client_id: &str,
    session_id: Option<&str>,
    command: FrontendCommand,
) -> FrontendCommandEnvelope {
    FrontendCommandEnvelope {
        protocol_version: FRONTEND_PROTOCOL_VERSION,
        command_id: format!("command-{sequence}"),
        idempotency_key: format!("idempotency-{sequence}"),
        frontend: FrontendKind::Desktop,
        client_id: client_id.to_owned(),
        session_id: session_id.map(str::to_owned),
        turn_id: None,
        timestamp: datetime!(2026-08-01 00:00 UTC),
        command,
    }
}

fn create_session(repo: &std::path::Path) -> String {
    AgentEngine::new(UnusedProvider, Config::default())
        .create_session(repo, "Frontend control coverage".to_owned())
        .expect("create durable session")
        .id
        .to_string()
}

#[test]
fn resumed_owner_can_drive_frontend_control_commands_idempotently() {
    let repo = tempdir().expect("temporary repository");
    let session_id = create_session(repo.path());
    let mut control = FrontendControlPlane::new(repo.path().to_path_buf(), Config::default());

    let list = envelope(1, "desktop-owner", None, FrontendCommand::ListSessions);
    let acknowledgement = control.dispatch(list.clone()).expect("list sessions");
    assert!(matches!(
        acknowledgement.result,
        FrontendControlResult::Sessions { ref sessions }
            if sessions.iter().any(|session| session.id == session_id)
    ));
    assert_eq!(
        control.dispatch(list).expect("replay idempotent command"),
        acknowledgement
    );

    let conflicting = FrontendCommandEnvelope {
        command: FrontendCommand::ShowStatus,
        ..envelope(
            1,
            "desktop-owner",
            Some(&session_id),
            FrontendCommand::ListSessions,
        )
    };
    let polling_reuse = control
        .dispatch(conflicting)
        .expect("polling commands do not reserve idempotency keys");
    assert!(matches!(
        polling_reuse.result,
        FrontendControlResult::Status {
            runtime_active: false,
            ..
        }
    ));

    let resumed = control
        .dispatch(envelope(
            2,
            "desktop-owner",
            Some(&session_id),
            FrontendCommand::ResumeSession {
                session_id: session_id.clone(),
            },
        ))
        .expect("resume session");
    assert!(matches!(
        resumed.result,
        FrontendControlResult::RuntimeReady { .. }
    ));
    let resumed_again = control
        .dispatch(envelope(
            3,
            "desktop-owner",
            Some(&session_id),
            FrontendCommand::ResumeSession {
                session_id: session_id.clone(),
            },
        ))
        .expect("resume existing daemon runtime");
    assert!(matches!(
        resumed_again.result,
        FrontendControlResult::RuntimeReady { .. }
    ));
    let attached_again = control
        .dispatch(envelope(
            4,
            "desktop-owner",
            Some(&session_id),
            FrontendCommand::Attach {
                session_id: session_id.clone(),
                mode: AttachmentMode::Owner,
                after_cursor: Some(0),
            },
        ))
        .expect("refresh owner attachment");
    assert!(matches!(
        attached_again.result,
        FrontendControlResult::Attached { .. }
    ));

    let status = control
        .dispatch(envelope(
            5,
            "desktop-owner",
            Some(&session_id),
            FrontendCommand::ShowStatus,
        ))
        .expect("show status");
    assert!(matches!(
        status.result,
        FrontendControlResult::Status {
            runtime_active: true,
            ..
        }
    ));

    let commands = [
        FrontendCommand::ConfigureModel {
            provider: Some("anthropic".to_owned()),
            model: "coverage-model".to_owned(),
            base_url: None,
        },
        FrontendCommand::SetEffort {
            effort: "low".to_owned(),
        },
        FrontendCommand::SetEffort {
            effort: "medium".to_owned(),
        },
        FrontendCommand::SetEffort {
            effort: "high".to_owned(),
        },
        FrontendCommand::SetEffort {
            effort: "auto".to_owned(),
        },
        FrontendCommand::SetPlanMode { enabled: true },
        FrontendCommand::SetPlanMode { enabled: false },
    ];
    for (offset, command) in commands.into_iter().enumerate() {
        let result = control
            .dispatch(envelope(
                10 + u32::try_from(offset).expect("small command offset"),
                "desktop-owner",
                Some(&session_id),
                command,
            ))
            .expect("authorized command");
        assert!(matches!(
            result.result,
            FrontendControlResult::CommandAccepted { .. }
        ));
    }

    assert!(matches!(
        control.dispatch(envelope(
            30,
            "desktop-owner",
            Some(&session_id),
            FrontendCommand::SetEffort {
                effort: "impossible".to_owned(),
            },
        )),
        Err(FrontendControlError::InvalidEffort(ref effort)) if effort == "impossible"
    ));
    for (offset, command) in [
        FrontendCommand::SteerWorker {
            worker_id: "worker-1".to_owned(),
            instruction: "inspect the fixture".to_owned(),
        },
        FrontendCommand::CancelWorker {
            worker_id: "worker-1".to_owned(),
        },
        FrontendCommand::StopTeam,
    ]
    .into_iter()
    .enumerate()
    {
        assert!(matches!(
            control.dispatch(envelope(
                35 + u32::try_from(offset).expect("small command offset"),
                "desktop-owner",
                Some(&session_id),
                command,
            )),
            Err(FrontendControlError::Runtime(_))
        ));
    }
    assert!(matches!(
        control
            .dispatch(envelope(
                31,
                "desktop-owner",
                Some(&session_id),
                FrontendCommand::CancelTurn,
            ))
            .expect("request cancellation")
            .result,
        FrontendControlResult::CancellationRequested { .. }
    ));

    let replay = control
        .replay_events("desktop-owner", 0)
        .expect("replay attached client events");
    let cursor = replay.next_cursor.max(1);
    assert!(matches!(
        control
            .dispatch(envelope(
                32,
                "desktop-owner",
                Some(&session_id),
                FrontendCommand::AcknowledgeCursor { cursor },
            ))
            .expect("acknowledge replay cursor")
            .result,
        FrontendControlResult::CursorAcknowledged { .. }
    ));
    assert!(matches!(
        control
            .dispatch(envelope(
                33,
                "desktop-owner",
                Some(&session_id),
                FrontendCommand::Detach,
            ))
            .expect("detach owner frontend")
            .result,
        FrontendControlResult::Detached { .. }
    ));
    assert!(control.replay_events("desktop-owner", 0).is_err());
}

#[test]
fn artifacts_and_read_only_frontends_fail_closed_without_provider_calls() {
    let repo = tempdir().expect("temporary repository");
    let session_id = create_session(repo.path());
    let mut control = FrontendControlPlane::new(repo.path().to_path_buf(), Config::default());

    let artifact_id = control
        .ingest_attachment(
            "notes.txt".to_owned(),
            Some("text/plain".to_owned()),
            b"deterministic coverage".to_vec(),
        )
        .expect("ingest attachment");
    let exported = control
        .export_attachment(&artifact_id)
        .expect("export attachment");
    assert_eq!(exported.display_name, "notes.txt");
    assert_eq!(exported.mime_type.as_deref(), Some("text/plain"));
    assert_eq!(exported.bytes, b"deterministic coverage");
    assert!(control.export_attachment("not-an-artifact").is_err());

    assert!(matches!(
        control.dispatch(envelope(
            40,
            "desktop-owner",
            None,
            FrontendCommand::CreateSession {
                repository_profile: "default".to_owned(),
                objective: Some("   ".to_owned()),
                attachment_ids: Vec::new(),
            },
        )),
        Err(FrontendControlError::ObjectiveRequired)
    ));
    assert!(matches!(
        control.dispatch(envelope(
            41,
            "desktop-owner",
            None,
            FrontendCommand::ShowStatus,
        )),
        Err(FrontendControlError::SessionRequired)
    ));
    assert!(matches!(
        control.dispatch(envelope(
            42,
            "desktop-owner",
            Some(&session_id),
            FrontendCommand::Attach {
                session_id: session_id.clone(),
                mode: AttachmentMode::Owner,
                after_cursor: None,
            },
        )),
        Err(FrontendControlError::RuntimeNotActive(ref inactive)) if inactive == &session_id
    ));

    control
        .dispatch(envelope(
            43,
            "desktop-owner",
            Some(&session_id),
            FrontendCommand::ResumeSession {
                session_id: session_id.clone(),
            },
        ))
        .expect("resume owner runtime");
    assert!(matches!(
        control
            .dispatch(envelope(
                44,
                "desktop-reader",
                Some(&session_id),
                FrontendCommand::Attach {
                    session_id: session_id.clone(),
                    mode: AttachmentMode::ReadOnly,
                    after_cursor: Some(0),
                },
            ))
            .expect("attach read-only frontend")
            .result,
        FrontendControlResult::Attached { .. }
    ));
    control
        .replay_events("desktop-reader", 0)
        .expect("read-only replay");

    let read_only_commands = [
        FrontendCommand::Submit {
            text: "must not run".to_owned(),
            attachment_ids: Vec::new(),
        },
        FrontendCommand::AnswerQuestion {
            question_id: "question-1".to_owned(),
            answer: "must not run".to_owned(),
        },
        FrontendCommand::ResolveApproval {
            approval_id: "approval-1".to_owned(),
            decision: ApprovalDecision::ApproveOnce,
        },
        FrontendCommand::ResolveApproval {
            approval_id: "approval-2".to_owned(),
            decision: ApprovalDecision::Deny,
        },
        FrontendCommand::CancelTurn,
    ];
    for (offset, command) in read_only_commands.into_iter().enumerate() {
        assert!(matches!(
            control.dispatch(envelope(
                50 + u32::try_from(offset).expect("small command offset"),
                "desktop-reader",
                Some(&session_id),
                command,
            )),
            Err(FrontendControlError::ReadOnlyClient(ref client)) if client == "desktop-reader"
        ));
    }

    let mut invalid = envelope(60, "desktop-reader", None, FrontendCommand::ListSessions);
    invalid.command_id.clear();
    assert!(matches!(
        control.dispatch(invalid),
        Err(FrontendControlError::InvalidEnvelope(_))
    ));
}
