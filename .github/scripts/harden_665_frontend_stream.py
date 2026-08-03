from pathlib import Path


def replace_once(text: str, old: str, new: str, label: str) -> str:
    count = text.count(old)
    if count != 1:
        raise SystemExit(f"expected exactly one {label}, found {count}")
    return text.replace(old, new, 1)

cli_path = Path("crates/medusa-cli/src/main.rs")
text = cli_path.read_text()
text = replace_once(
    text,
    "            Some(RuntimeEvent::Failed(error)) if runtime.active_session_id().is_none() => {",
    "            Some(RuntimeEvent::Failed(error))\n                if runtime.active_session_id().is_none()\n                    || is_unjournaled_runtime_failure(&error) =>\n            {",
    "headless unjournaled failure guard",
)
text = replace_once(
    text,
    "fn request_daemon_shutdown(repo: &Path) {",
    '''fn is_unjournaled_runtime_failure(message: &str) -> bool {
    message.starts_with("runtime event was not published because")
}

fn request_daemon_shutdown(repo: &Path) {''',
    "unjournaled failure helper",
)
needle = '''    #[test]
    fn hidden_daemon_host_accepts_repository_after_subcommand() {
        let cli = Cli::try_parse_from(["medusa", "__daemon-serve", "--repo", "."])
            .expect("parse daemon host");
        assert!(matches!(cli.command, Some(CommandKind::DaemonServe)));
    }
}'''
replacement = '''    #[test]
    fn hidden_daemon_host_accepts_repository_after_subcommand() {
        let cli = Cli::try_parse_from(["medusa", "__daemon-serve", "--repo", "."])
            .expect("parse daemon host");
        assert!(matches!(cli.command, Some(CommandKind::DaemonServe)));
    }

    #[test]
    fn headless_runtime_fails_closed_when_terminal_publication_is_not_journaled() {
        assert!(is_unjournaled_runtime_failure(
            "runtime event was not published because its durable record failed: disk full"
        ));
        assert!(is_unjournaled_runtime_failure(
            "runtime event was not published because durable serialization failed: invalid value"
        ));
        assert!(!is_unjournaled_runtime_failure(
            "provider returned an ordinary session-bound failure"
        ));
    }
}'''
text = replace_once(text, needle, replacement, "CLI fail-closed regression test")
cli_path.write_text(text)

frontend_path = Path("crates/medusa-runtime/src/frontend.rs")
text = frontend_path.read_text()
text += r'''

#[cfg(test)]
mod tests {
    use std::path::Path;

    use medusa_agent::{AgentSession, record_session_event};
    use medusa_core::SessionId;
    use medusa_protocol::{
        Actor, EventPayload,
        frontend::{FrontendEvent, FrontendKind},
    };
    use serde_json::json;
    use tempfile::tempdir;
    use time::OffsetDateTime;

    use super::CanonicalFrontendEventStream;

    fn durable_session(repo: &Path) -> AgentSession {
        AgentSession {
            id: SessionId::new(),
            objective: "canonical frontend replay".to_owned(),
            repo: repo.to_path_buf(),
            created_at: OffsetDateTime::UNIX_EPOCH,
            updated_at: OffsetDateTime::UNIX_EPOCH,
            completed: false,
            turn: 0,
            plan: Vec::new(),
            pending_question: None,
            messages: Vec::new(),
            events: Vec::new(),
            evidence: Vec::new(),
            tool_artifacts: Vec::new(),
            world_model: None,
            approval_grants: Vec::new(),
            approval_receipts: Vec::new(),
            rollback_receipts: Vec::new(),
        }
    }

    #[test]
    fn stream_advances_the_canonical_cursor_through_non_presentable_events() {
        let directory = tempdir().expect("temporary repository");
        let mut session = durable_session(directory.path());
        let objective = session.objective.clone();
        record_session_event(
            &mut session,
            Actor::Coordinator,
            EventPayload::SessionCreated { objective },
        )
        .expect("persist session creation");
        record_session_event(
            &mut session,
            Actor::Coordinator,
            EventPayload::AssistantMessageRecorded {
                message: json!({
                    "role": "user",
                    "content": [{"type": "text", "text": "not assistant-visible"}],
                }),
            },
        )
        .expect("persist non-presentable event");
        record_session_event(
            &mut session,
            Actor::Coordinator,
            EventPayload::RuntimeTurnFinished,
        )
        .expect("persist terminal event");

        let session_id = session.id.to_string();
        let mut stream = CanonicalFrontendEventStream::new(
            directory.path().to_path_buf(),
            FrontendKind::Headless,
        );
        let accepted = stream
            .try_event(&session_id)
            .expect("replay accepted event")
            .expect("accepted event");
        assert!(matches!(accepted.event, FrontendEvent::SubmissionAccepted));
        assert_eq!(accepted.cursor, 1);
        assert!(accepted.event_id.ends_with(":headless"));

        let finished = stream
            .try_event(&session_id)
            .expect("replay terminal event")
            .expect("terminal event");
        assert!(matches!(finished.event, FrontendEvent::TurnFinished));
        assert_eq!(finished.cursor, 3);
        assert_eq!(stream.journal_cursor(), 3);
        assert!(
            stream
                .try_event(&session_id)
                .expect("replay exhausted")
                .is_none()
        );
    }
}
'''
frontend_path.write_text(text)
