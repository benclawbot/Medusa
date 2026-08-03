from __future__ import annotations

from pathlib import Path
import subprocess

EXPECTED_BLOBS = {
    "crates/medusa-daemon/src/lib.rs": "ae1102df9c254bc5662afbdce333ad5afd0e2269",
    "crates/medusa-daemon/src/live_session.rs": "fd1ca8f73af05c06c3f27a8959b21b0307bfbb82",
    "crates/medusa-daemon/src/frontend_control.rs": "26364a836de2150e3b920c7e5ba7b2711216a900",
    "docs/architecture/INDEX.md": "4502b5b18b5160f6377de0266307828db6acf5ea",
    "docs/architecture/decisions/0007-canonical-frontend-projection.md": "41c32037f7f05c65cde61c8700a3a7acdb238fe4",
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

live_path = Path("crates/medusa-daemon/src/live_session.rs")
live = live_path.read_text(encoding="utf-8")
live = replace_once(
    live,
    "use medusa_protocol::EventEnvelope;\n",
    "use medusa_protocol::{\n"
    "    EventEnvelope,\n"
    "    frontend::{FrontendEventEnvelope, FrontendKind, project_event},\n"
    "};\n",
    str(live_path),
)
live = replace_once(
    live,
    "/// Current daemon view of one attached frontend client.\n"
    "#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]\n"
    "pub struct LiveSessionAttachmentView {\n"
    "    pub session: LiveSessionSummary,\n"
    "    pub client_id: String,\n"
    "    pub client_kind: ClientKind,\n"
    "    pub mode: AttachmentMode,\n"
    "    pub continuity_revision: u64,\n"
    "    pub acknowledged_cursor: u64,\n"
    "    pub owner_client_id: Option<String>,\n"
    "    pub replay: Vec<EventEnvelope>,\n"
    "}\n",
    "/// One frontend-scoped replay batch over an authoritative journal range.\n"
    "#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]\n"
    "pub struct LiveSessionReplayView {\n"
    "    pub session_id: String,\n"
    "    pub client_id: String,\n"
    "    pub frontend: FrontendKind,\n"
    "    pub after_cursor: u64,\n"
    "    pub next_cursor: u64,\n"
    "    pub events: Vec<FrontendEventEnvelope>,\n"
    "}\n\n"
    "/// Current daemon view of one attached frontend client.\n"
    "#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]\n"
    "pub struct LiveSessionAttachmentView {\n"
    "    pub session: LiveSessionSummary,\n"
    "    pub client_id: String,\n"
    "    pub client_kind: ClientKind,\n"
    "    pub frontend: FrontendKind,\n"
    "    pub mode: AttachmentMode,\n"
    "    pub continuity_revision: u64,\n"
    "    pub acknowledged_cursor: u64,\n"
    "    pub replay_cursor: u64,\n"
    "    pub owner_client_id: Option<String>,\n"
    "    pub replay: Vec<FrontendEventEnvelope>,\n"
    "}\n",
    str(live_path),
)
live = replace_once(
    live,
    "    pub fn replay(\n"
    "        &self,\n"
    "        client_id: &str,\n"
    "        cursor: u64,\n"
    "    ) -> Result<Vec<EventEnvelope>, LiveSessionBrokerError> {\n"
    "        self.attachment(client_id)?\n"
    "            .replay_from(cursor)\n"
    "            .map_err(Into::into)\n"
    "    }\n",
    "    pub fn replay(\n"
    "        &self,\n"
    "        client_id: &str,\n"
    "        cursor: u64,\n"
    "    ) -> Result<LiveSessionReplayView, LiveSessionBrokerError> {\n"
    "        let attachment = self.attachment(client_id)?;\n"
    "        let client_kind = attachment\n"
    "            .continuity\n"
    "            .attachments\n"
    "            .iter()\n"
    "            .find(|candidate| candidate.client_id == client_id)\n"
    "            .map(|candidate| candidate.client_kind.clone())\n"
    "            .ok_or_else(|| LiveSessionBrokerError::ClientNotAttached(client_id.to_owned()))?;\n"
    "        let replay = attachment.replay_from(cursor)?;\n"
    "        Ok(replay_view(attachment, &client_kind, cursor, replay))\n"
    "    }\n",
    str(live_path),
)
old_view = '''fn attachment_view(
    attachment: &RuntimeSessionAttachment,
) -> Result<LiveSessionAttachmentView, LiveSessionBrokerError> {
    let metadata = attachment
        .continuity
        .attachments
        .iter()
        .find(|candidate| candidate.client_id == attachment.client_id())
        .ok_or_else(|| {
            LiveSessionBrokerError::ClientNotAttached(attachment.client_id().to_owned())
        })?;
    Ok(LiveSessionAttachmentView {
        session: LiveSessionSummary {
            id: attachment.session.id.to_string(),
            objective: attachment.session.objective.clone(),
            created_at: attachment.session.created_at,
            updated_at: attachment.session.updated_at,
            completed: attachment.session.completed,
            waiting_for_user: attachment.session.pending_question.is_some(),
            turn: attachment.session.turn,
        },
        client_id: attachment.client_id().to_owned(),
        client_kind: metadata.client_kind.clone(),
        mode: attachment.mode(),
        continuity_revision: attachment.continuity.revision,
        acknowledged_cursor: metadata.journal_cursor,
        owner_client_id: attachment.continuity.owner_client_id.clone(),
        replay: attachment.replay.clone(),
    })
}
'''
new_view = '''fn attachment_view(
    attachment: &RuntimeSessionAttachment,
) -> Result<LiveSessionAttachmentView, LiveSessionBrokerError> {
    let metadata = attachment
        .continuity
        .attachments
        .iter()
        .find(|candidate| candidate.client_id == attachment.client_id())
        .ok_or_else(|| {
            LiveSessionBrokerError::ClientNotAttached(attachment.client_id().to_owned())
        })?;
    let frontend = frontend_kind(&metadata.client_kind);
    let replay_cursor = attachment
        .replay
        .last()
        .map_or(metadata.journal_cursor, |event| event.sequence);
    Ok(LiveSessionAttachmentView {
        session: LiveSessionSummary {
            id: attachment.session.id.to_string(),
            objective: attachment.session.objective.clone(),
            created_at: attachment.session.created_at,
            updated_at: attachment.session.updated_at,
            completed: attachment.session.completed,
            waiting_for_user: attachment.session.pending_question.is_some(),
            turn: attachment.session.turn,
        },
        client_id: attachment.client_id().to_owned(),
        client_kind: metadata.client_kind.clone(),
        frontend,
        mode: attachment.mode(),
        continuity_revision: attachment.continuity.revision,
        acknowledged_cursor: metadata.journal_cursor,
        replay_cursor,
        owner_client_id: attachment.continuity.owner_client_id.clone(),
        replay: project_replay(&attachment.replay, frontend),
    })
}

fn replay_view(
    attachment: &RuntimeSessionAttachment,
    client_kind: &ClientKind,
    after_cursor: u64,
    replay: Vec<EventEnvelope>,
) -> LiveSessionReplayView {
    let frontend = frontend_kind(client_kind);
    let next_cursor = replay.last().map_or(after_cursor, |event| event.sequence);
    LiveSessionReplayView {
        session_id: attachment.session.id.to_string(),
        client_id: attachment.client_id().to_owned(),
        frontend,
        after_cursor,
        next_cursor,
        events: project_replay(&replay, frontend),
    }
}

fn project_replay(
    replay: &[EventEnvelope],
    frontend: FrontendKind,
) -> Vec<FrontendEventEnvelope> {
    replay
        .iter()
        .filter_map(|event| project_event(event, event.sequence, frontend))
        .collect()
}

const fn frontend_kind(client_kind: &ClientKind) -> FrontendKind {
    match client_kind {
        ClientKind::Tui => FrontendKind::Tui,
        ClientKind::Desktop => FrontendKind::Desktop,
        ClientKind::Telegram => FrontendKind::Telegram,
        ClientKind::Daemon | ClientKind::Other(_) => FrontendKind::Other,
    }
}
'''
live = replace_once(live, old_view, new_view, str(live_path))
live = replace_once(
    live,
    "    use medusa_agent::AgentEngine;\n"
    "    use medusa_config::Config;\n"
    "    use medusa_core::MedusaResult;\n"
    "    use medusa_provider::{ModelProvider, ModelRequest, ModelResponse};\n",
    "    use medusa_agent::{AgentEngine, record_session_event};\n"
    "    use medusa_config::Config;\n"
    "    use medusa_core::MedusaResult;\n"
    "    use medusa_protocol::{\n"
    "        Actor, EventPayload,\n"
    "        frontend::{FrontendEvent, FrontendKind},\n"
    "    };\n"
    "    use medusa_provider::{ModelProvider, ModelRequest, ModelResponse};\n"
    "    use serde_json::json;\n",
    str(live_path),
)
live = replace_once(
    live,
    "        assert_eq!(owner.session.id, observer.session.id);\n"
    "        assert_eq!(owner.replay, observer.replay);\n\n"
    "        let cursor = u64::try_from(observer.replay.len()).expect(\"cursor\");\n",
    "        assert_eq!(owner.session.id, observer.session.id);\n"
    "        assert_eq!(owner.frontend, FrontendKind::Tui);\n"
    "        assert_eq!(observer.frontend, FrontendKind::Telegram);\n"
    "        assert_eq!(owner.replay_cursor, observer.replay_cursor);\n"
    "        assert_eq!(owner.replay.len(), observer.replay.len());\n"
    "        for (owner_event, observer_event) in owner.replay.iter().zip(&observer.replay) {\n"
    "            assert_eq!(owner_event.cursor, observer_event.cursor);\n"
    "            assert_eq!(owner_event.event, observer_event.event);\n"
    "            assert!(owner_event.event_id.ends_with(\":tui\"));\n"
    "            assert!(observer_event.event_id.ends_with(\":telegram\"));\n"
    "        }\n\n"
    "        let cursor = observer.replay_cursor;\n",
    str(live_path),
)
live = replace_once(
    live,
    "        assert_eq!(reattached.acknowledged_cursor, cursor);\n"
    "        assert!(reattached.replay.is_empty());\n"
    "    }\n\n"
    "    #[test]\n"
    "    fn handoff_changes_owner_without_forking_the_session() {\n",
    "        assert_eq!(reattached.acknowledged_cursor, cursor);\n"
    "        assert_eq!(reattached.replay_cursor, cursor);\n"
    "        assert!(reattached.replay.is_empty());\n"
    "    }\n\n"
    "    #[test]\n"
    "    fn replay_cursor_advances_through_non_presentable_events() {\n"
    "        let repository = tempfile::tempdir().expect(\"repository\");\n"
    "        let mut session = AgentEngine::new(UnusedProvider, Config::default())\n"
    "            .create_session(repository.path(), \"Hidden replay event\".to_owned())\n"
    "            .expect(\"session\");\n"
    "        record_session_event(\n"
    "            &mut session,\n"
    "            Actor::Coordinator,\n"
    "            EventPayload::AssistantMessageRecorded {\n"
    "                message: json!({\n"
    "                    \"role\": \"user\",\n"
    "                    \"content\": [{\"type\": \"text\", \"text\": \"not frontend-visible\"}],\n"
    "                }),\n"
    "            },\n"
    "        )\n"
    "        .expect(\"persist hidden event\");\n"
    "        let session_id = session.id.to_string();\n"
    "        let mut broker = LiveSessionBroker::new(repository.path().to_path_buf());\n"
    "        let attached = broker\n"
    "            .attach(request(\n"
    "                &session_id,\n"
    "                \"desktop-observer\",\n"
    "                ClientKind::Desktop,\n"
    "                AttachmentMode::ReadOnly,\n"
    "                0,\n"
    "                1,\n"
    "                \"attach-hidden\",\n"
    "            ))\n"
    "            .expect(\"attach\");\n"
    "        assert_eq!(attached.frontend, FrontendKind::Desktop);\n"
    "        assert_eq!(attached.acknowledged_cursor, 1);\n"
    "        assert_eq!(attached.replay_cursor, 2);\n"
    "        assert!(attached.replay.is_empty());\n\n"
    "        let replay = broker\n"
    "            .replay(\"desktop-observer\", 1)\n"
    "            .expect(\"replay\");\n"
    "        assert_eq!(replay.frontend, FrontendKind::Desktop);\n"
    "        assert_eq!(replay.after_cursor, 1);\n"
    "        assert_eq!(replay.next_cursor, 2);\n"
    "        assert!(replay.events.is_empty());\n"
    "        let acknowledged = broker\n"
    "            .acknowledge_cursor(\n"
    "                \"desktop-observer\",\n"
    "                replay.next_cursor,\n"
    "                30_003,\n"
    "                \"ack-hidden\",\n"
    "            )\n"
    "            .expect(\"ack hidden cursor\");\n"
    "        assert_eq!(acknowledged.acknowledged_cursor, 2);\n"
    "    }\n\n"
    "    #[test]\n"
    "    fn handoff_changes_owner_without_forking_the_session() {\n",
    str(live_path),
)
live_path.write_text(live, encoding="utf-8")

control_path = Path("crates/medusa-daemon/src/frontend_control.rs")
control = control_path.read_text(encoding="utf-8")
control = replace_once(
    control,
    "use medusa_protocol::{\n"
    "    EventEnvelope,\n"
    "    frontend::{\n"
    "        ApprovalDecision, AttachmentMode as FrontendAttachmentMode, FrontendCommand,\n"
    "        FrontendCommandEnvelope, FrontendKind,\n"
    "    },\n"
    "};\n",
    "use medusa_protocol::frontend::{\n"
    "    ApprovalDecision, AttachmentMode as FrontendAttachmentMode, FrontendCommand,\n"
    "    FrontendCommandEnvelope, FrontendKind,\n"
    "};\n",
    str(control_path),
)
control = replace_once(
    control,
    "        LiveSessionAttachmentView, LiveSessionBroker, LiveSessionBrokerError, LiveSessionSummary,\n",
    "        LiveSessionAttachmentView, LiveSessionBroker, LiveSessionBrokerError,\n"
    "        LiveSessionReplayView, LiveSessionSummary,\n",
    str(control_path),
)
control = replace_once(
    control,
    "    Events {\n"
    "        session_id: String,\n"
    "        after_cursor: u64,\n"
    "        events: Vec<EventEnvelope>,\n"
    "    },\n",
    "    Events {\n"
    "        replay: LiveSessionReplayView,\n"
    "    },\n",
    str(control_path),
)
control = replace_once(
    control,
    "    ) -> Result<Vec<EventEnvelope>, FrontendControlError> {\n",
    "    ) -> Result<LiveSessionReplayView, FrontendControlError> {\n",
    str(control_path),
)
control_path.write_text(control, encoding="utf-8")

lib_path = Path("crates/medusa-daemon/src/lib.rs")
lib = lib_path.read_text(encoding="utf-8")
lib = replace_once(
    lib,
    "    LiveSessionAttachmentView, LiveSessionBroker, LiveSessionBrokerError, LiveSessionSummary,\n",
    "    LiveSessionAttachmentView, LiveSessionBroker, LiveSessionBrokerError, LiveSessionReplayView,\n"
    "    LiveSessionSummary,\n",
    str(lib_path),
)
lib_path.write_text(lib, encoding="utf-8")

index_path = Path("docs/architecture/INDEX.md")
index = index_path.read_text(encoding="utf-8")
index = replace_once(
    index,
    "| Daemon service | `medusa __daemon-serve` | `crates/medusa-daemon` | `medusa-runtime::RuntimeController` |",
    "| Daemon service | `medusa __daemon-serve` | `crates/medusa-daemon` | daemon-owned runtime and continuity; canonical journal → frontend-scoped replay batches |",
    str(index_path),
)
index = replace_once(
    index,
    "The phase-6 frontend migration is proceeding in production-entrypoint order. Headless CLI and interactive TUI transcript/lifecycle output now tail committed session-journal events through the versioned `medusa-protocol::frontend` projection. The TUI temporarily retains process-local settings, startup recovery, turn-counter, and explicit reset hints; daemon attachment/replay, desktop, and remote voice surfaces remain follow-up slices.",
    "The phase-6 frontend migration is proceeding in production-entrypoint order. Headless CLI and interactive TUI output tail committed session-journal events through `medusa-protocol::frontend`. Daemon attachment and replay now return the same frontend-scoped envelopes plus an explicit next canonical cursor that advances through non-presentable events. The TUI temporarily retains local settings, startup recovery, turn-counter, and reset hints; daemon wire integration, desktop, and remote voice surfaces remain follow-up slices.",
    str(index_path),
)
index_path.write_text(index, encoding="utf-8")

adr_path = Path("docs/architecture/decisions/0007-canonical-frontend-projection.md")
adr = adr_path.read_text(encoding="utf-8")
adr = replace_once(
    adr,
    "The headless CLI and interactive TUI now consume the canonical stream for durable transcript, plan, question, activity, usage, cancellation, failure, and completion state. The TUI keeps process-local settings, startup recovery, turn-counter, and explicit reset hints only as bounded compatibility inputs while daemon attachment/replay is built.",
    "The headless CLI and interactive TUI consume the canonical stream for durable transcript, plan, question, activity, usage, cancellation, failure, and completion state. Daemon attachments and replay project the same journal range according to each attached frontend kind and expose a next canonical cursor even when every scanned event is non-presentable. The TUI keeps process-local settings, startup recovery, turn-counter, and reset hints only as bounded compatibility inputs while daemon wire integration is completed.",
    str(adr_path),
)
adr_path.write_text(adr, encoding="utf-8")
