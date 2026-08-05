#!/usr/bin/env python3
"""Apply and validate the final durable-completion fix for PR #682."""

from __future__ import annotations

import subprocess
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
RUNTIME = ROOT / "crates/medusa-runtime/src/lib.rs"
WORKFLOW = ROOT / ".github/workflows/pr-682-finalize.yml"
SCRIPT = Path(__file__).resolve()


def run(*args: str) -> None:
    subprocess.run(args, cwd=ROOT, check=True)


def replace_once(source: str, old: str, new: str, label: str) -> str:
    count = source.count(old)
    if count != 1:
        raise RuntimeError(f"expected exactly one {label} match, found {count}")
    return source.replace(old, new, 1)


def main() -> None:
    source = RUNTIME.read_text(encoding="utf-8")
    source = replace_once(
        source,
        '''                            medusa_agent::record_session_event(
                                &mut session,
                                Actor::Coordinator,
                                EventPayload::AssistantMessageRecorded {
                                    message: serde_json::to_value(&message)
                                        .map_err(RuntimeError::agent)?,
                                },
                            )
                            .map_err(RuntimeError::agent)?;
                            session.completed = true;
                            let _ = events.send(RuntimeEvent::AssistantText(completion_text));
''',
        '''                            medusa_agent::record_session_event(
                                &mut session,
                                Actor::Coordinator,
                                EventPayload::AssistantMessageRecorded {
                                    message: serde_json::to_value(&message)
                                        .map_err(RuntimeError::agent)?,
                                },
                            )
                            .map_err(RuntimeError::agent)?;
                            session.completed = true;
                            medusa_agent::record_session_event(
                                &mut session,
                                Actor::Coordinator,
                                EventPayload::SessionCompleted {
                                    report_ref: format!("commit:{}", receipt.commit),
                                },
                            )
                            .map_err(RuntimeError::agent)?;
                            let _ = events.send(RuntimeEvent::AssistantText(completion_text));
''',
        "dedicated completion persistence",
    )
    source = replace_once(
        source,
        '''    fn falls_back_to_verified_status_when_summary_has_no_visible_text() {
        let text = mutation_completion_text(
            "<think>private implementation reasoning</think>",
            "abc123",
            &["src/lib.rs".to_owned()],
        );
        assert_eq!(
            text,
            "Verified and integrated commit `abc123`. Changed paths: src/lib.rs."
        );
    }
}
''',
        '''    fn falls_back_to_verified_status_when_summary_has_no_visible_text() {
        let text = mutation_completion_text(
            "<think>private implementation reasoning</think>",
            "abc123",
            &["src/lib.rs".to_owned()],
        );
        assert_eq!(
            text,
            "Verified and integrated commit `abc123`. Changed paths: src/lib.rs."
        );
    }

    #[test]
    fn durable_completion_event_marks_the_session_completed() {
        use medusa_agent::AgentSession;
        use medusa_core::SessionId;
        use medusa_protocol::{Actor, EventPayload};
        use time::OffsetDateTime;

        let directory = tempfile::tempdir().expect("temporary repository");
        let mut session = AgentSession {
            id: SessionId::new(),
            objective: "durable mutation completion".to_owned(),
            repo: directory.path().to_path_buf(),
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
        };
        session.completed = true;
        medusa_agent::record_session_event(
            &mut session,
            Actor::Coordinator,
            EventPayload::SessionCompleted {
                report_ref: "commit:abc123".to_owned(),
            },
        )
        .expect("persist completion");

        let persisted = medusa_agent::session_browser::load_session(
            directory.path(),
            session.id.as_str(),
        )
        .expect("reload completed session");
        assert!(persisted.completed);
        assert!(matches!(
            persisted.events.last().map(|event| &event.payload),
            Some(EventPayload::SessionCompleted { report_ref }) if report_ref == "commit:abc123"
        ));
    }
}
''',
        "durable completion regression test insertion",
    )
    RUNTIME.write_text(source, encoding="utf-8")

    run("cargo", "fmt", "--all")
    run(
        "cargo",
        "test",
        "-p",
        "medusa-runtime",
        "mutation_completion_tests",
        "--",
        "--nocapture",
    )
    run(
        "cargo",
        "clippy",
        "-p",
        "medusa-runtime",
        "--all-targets",
        "--",
        "-D",
        "warnings",
    )
    run("python3", "scripts/check-mutation-lifecycle.py")
    run("python3", "scripts/check-evidence-authority.py")

    WORKFLOW.unlink(missing_ok=True)
    SCRIPT.unlink(missing_ok=True)
    run("git", "add", "-A")
    run("git", "diff", "--cached", "--check")
    run("git", "commit", "-m", "Persist dedicated mutation completion")
    run("git", "push", "origin", "HEAD:agent/654-dedicated-parent-reviewer")


if __name__ == "__main__":
    main()
