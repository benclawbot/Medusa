use std::{path::Path, thread, time::Duration};

use medusa_agent::AgentEngine;
use medusa_config::Config;
use medusa_core::MedusaResult;
use medusa_daemon::{
    DaemonClient, DaemonPaths, FrontendControlResult, LiveSessionAttachmentView,
    LiveSessionReplayView, spawn,
};
use medusa_protocol::frontend::{
    FRONTEND_PROTOCOL_VERSION, AttachmentMode, FrontendCommand, FrontendCommandEnvelope,
    FrontendKind,
};
use medusa_provider::{ModelProvider, ModelRequest, ModelResponse};
use time::OffsetDateTime;

struct UnusedProvider;

impl ModelProvider for UnusedProvider {
    fn complete(&self, _: &ModelRequest) -> MedusaResult<ModelResponse> {
        unreachable!("session creation does not call the provider")
    }
}

fn wait_for_endpoint(path: &Path) {
    for _ in 0..200 {
        if path.exists() {
            return;
        }
        thread::sleep(Duration::from_millis(20));
    }
    panic!("daemon endpoint did not appear: {}", path.display());
}

fn envelope(
    frontend: FrontendKind,
    client_id: &str,
    command_id: &str,
    session_id: Option<&str>,
    command: FrontendCommand,
) -> FrontendCommandEnvelope {
    FrontendCommandEnvelope {
        protocol_version: FRONTEND_PROTOCOL_VERSION,
        command_id: command_id.to_owned(),
        idempotency_key: format!("{client_id}:{command_id}"),
        frontend,
        client_id: client_id.to_owned(),
        session_id: session_id.map(str::to_owned),
        turn_id: None,
        timestamp: OffsetDateTime::now_utc(),
        command,
    }
}

fn runtime_attachment(result: FrontendControlResult) -> LiveSessionAttachmentView {
    match result {
        FrontendControlResult::RuntimeReady { attachment }
        | FrontendControlResult::Attached { attachment } => attachment,
        other => panic!("expected runtime attachment, got {other:?}"),
    }
}

fn replay(result: FrontendControlResult) -> LiveSessionReplayView {
    match result {
        FrontendControlResult::Events { replay } => replay,
        other => panic!("expected replay, got {other:?}"),
    }
}

fn assert_equivalent_replay(left: &LiveSessionReplayView, right: &LiveSessionReplayView) {
    assert_eq!(left.session_id, right.session_id);
    assert_eq!(left.after_cursor, right.after_cursor);
    assert_eq!(left.next_cursor, right.next_cursor);
    assert_eq!(left.events.len(), right.events.len());
    for (left_event, right_event) in left.events.iter().zip(&right.events) {
        assert_eq!(left_event.cursor, right_event.cursor);
        assert_eq!(left_event.event, right_event.event);
    }
}

#[test]
fn simultaneous_frontends_share_daemon_ordering_and_control_authority() {
    let repository = tempfile::tempdir().expect("repository");
    let session = AgentEngine::new(UnusedProvider, Config::default())
        .create_session(repository.path(), "One daemon-authoritative session".to_owned())
        .expect("session");
    let session_id = session.id.to_string();
    let paths = DaemonPaths::for_repo(repository.path());
    let (handle, server) = spawn(paths.clone()).expect("spawn daemon");
    wait_for_endpoint(&paths.socket);

    let tui = DaemonClient::new(&paths.socket);
    let telegram = DaemonClient::new(&paths.socket);

    let tui_attachment = runtime_attachment(
        tui.frontend(envelope(
            FrontendKind::Tui,
            "tui-client",
            "resume-tui",
            Some(&session_id),
            FrontendCommand::ResumeSession {
                session_id: session_id.clone(),
            },
        ))
        .expect("resume session through TUI client")
        .result,
    );
    let telegram_attachment = runtime_attachment(
        telegram
            .frontend(envelope(
                FrontendKind::Telegram,
                "telegram-client",
                "attach-telegram",
                Some(&session_id),
                FrontendCommand::Attach {
                    session_id: session_id.clone(),
                    mode: AttachmentMode::ReadOnly,
                    after_cursor: Some(0),
                },
            ))
            .expect("attach Telegram observer")
            .result,
    );

    assert_eq!(tui_attachment.session.id, session_id);
    assert_eq!(telegram_attachment.session.id, session_id);
    assert_eq!(tui_attachment.replay_cursor, telegram_attachment.replay_cursor);
    assert_eq!(tui_attachment.replay.len(), telegram_attachment.replay.len());
    for (tui_event, telegram_event) in tui_attachment
        .replay
        .iter()
        .zip(&telegram_attachment.replay)
    {
        assert_eq!(tui_event.cursor, telegram_event.cursor);
        assert_eq!(tui_event.event, telegram_event.event);
    }

    let tui_replay = replay(
        tui.frontend(envelope(
            FrontendKind::Tui,
            "tui-client",
            "replay-tui",
            Some(&session_id),
            FrontendCommand::Replay { after_cursor: 0 },
        ))
        .expect("replay through TUI client")
        .result,
    );
    let telegram_replay = replay(
        telegram
            .frontend(envelope(
                FrontendKind::Telegram,
                "telegram-client",
                "replay-telegram",
                Some(&session_id),
                FrontendCommand::Replay { after_cursor: 0 },
            ))
            .expect("replay through Telegram client")
            .result,
    );
    assert_equivalent_replay(&tui_replay, &telegram_replay);

    let tui_status = tui
        .frontend(envelope(
            FrontendKind::Tui,
            "tui-client",
            "status-tui",
            Some(&session_id),
            FrontendCommand::ShowStatus,
        ))
        .expect("TUI status")
        .result;
    let telegram_status = telegram
        .frontend(envelope(
            FrontendKind::Telegram,
            "telegram-client",
            "status-telegram",
            Some(&session_id),
            FrontendCommand::ShowStatus,
        ))
        .expect("Telegram status")
        .result;
    assert_eq!(tui_status, telegram_status);

    let read_only_error = telegram
        .frontend(envelope(
            FrontendKind::Telegram,
            "telegram-client",
            "cancel-telegram",
            Some(&session_id),
            FrontendCommand::CancelTurn,
        ))
        .expect_err("read-only Telegram observer cannot mutate runtime control");
    assert!(
        read_only_error.to_string().contains("read-only"),
        "unexpected control error: {read_only_error}"
    );

    let detached = telegram
        .frontend(envelope(
            FrontendKind::Telegram,
            "telegram-client",
            "detach-telegram",
            Some(&session_id),
            FrontendCommand::Detach,
        ))
        .expect("detach Telegram observer")
        .result;
    assert!(matches!(
        detached,
        FrontendControlResult::Detached {
            ref session_id,
            ..
        } if session_id == &tui_attachment.session.id
    ));

    let status_after_detach = tui
        .frontend(envelope(
            FrontendKind::Tui,
            "tui-client",
            "status-after-detach",
            Some(&session_id),
            FrontendCommand::ShowStatus,
        ))
        .expect("runtime remains active after observer detach")
        .result;
    assert!(matches!(
        status_after_detach,
        FrontendControlResult::Status {
            runtime_active: true,
            ..
        }
    ));

    handle.shutdown();
    server.join().expect("join daemon").expect("daemon result");
}
