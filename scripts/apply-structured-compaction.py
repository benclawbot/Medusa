from pathlib import Path

protocol = Path("crates/medusa-protocol/src/lib.rs")
text = protocol.read_text()
old = """    ConversationCompacted {
        original_messages: u32,
        retained_messages: u32,
    },"""
new = """    ConversationCompacted {
        original_messages: u32,
        retained_messages: u32,
        #[serde(default)]
        generation: u32,
        #[serde(default)]
        source_event_sequences: Vec<u64>,
        #[serde(default)]
        preserved_sections: Vec<String>,
    },"""
assert old in text
protocol.write_text(text.replace(old, new, 1))

support = Path("crates/medusa-agent/src/engine_support.rs")
text = support.read_text()
start = text.index("/// Compacts durable session history without requiring a live model provider.\npub fn compact_session")
end = text.index("\npub(crate) fn compact_message_text", start)
replacement = r'''/// Compacts durable session history without requiring a live model provider.
pub fn compact_session(session: &mut AgentSession, focus: Option<&str>) -> MedusaResult<()> {
    const MARKER: &str = "[medusa-compaction-v1]";
    const MAX_ENTRIES: usize = 24;

    let original_messages = session.messages.len();
    let generation = u32::try_from(
        session
            .events
            .iter()
            .filter(|event| matches!(event.payload, EventPayload::ConversationCompacted { .. }))
            .count()
            .saturating_add(1),
    )
    .unwrap_or(u32::MAX);
    let source_event_sequences = session.events.iter().map(|event| event.sequence).collect::<Vec<_>>();
    let mut entries = session
        .messages
        .iter()
        .flat_map(|message| message.content.iter().map(move |block| (message.role, block)))
        .filter(|(_, block)| !matches!(block, MessageBlock::Text { text } if text.starts_with(MARKER)))
        .map(|(role, block)| {
            let speaker = match role {
                Role::User => "User",
                Role::Assistant => "Assistant",
            };
            format!("{speaker}: {}", compact_block_text(block))
        })
        .collect::<Vec<_>>();
    if entries.len() > MAX_ENTRIES {
        entries = entries.split_off(entries.len() - MAX_ENTRIES);
    }

    let focus = focus.filter(|value| !value.trim().is_empty()).map(str::trim).unwrap_or("none");
    let plan = serde_json::to_string(&session.plan).map_err(json_error)?;
    let pending_question = serde_json::to_string(&session.pending_question).map_err(json_error)?;
    let approval_grants = serde_json::to_string(&session.approval_grants).map_err(json_error)?;
    let approval_receipts = serde_json::to_string(&session.approval_receipts).map_err(json_error)?;
    let evidence = serde_json::to_string(&session.evidence).map_err(json_error)?;
    let artifacts = serde_json::to_string(
        &session
            .tool_artifacts
            .iter()
            .map(|path| path.display().to_string())
            .collect::<Vec<_>>(),
    )
    .map_err(json_error)?;
    let recent_context = if entries.is_empty() { "none".to_owned() } else { entries.join("\n") };
    let preserved_sections = vec![
        "objective".to_owned(),
        "focus".to_owned(),
        "plan".to_owned(),
        "pending_question".to_owned(),
        "approval_grants".to_owned(),
        "approval_receipts".to_owned(),
        "verification_evidence".to_owned(),
        "tool_artifacts".to_owned(),
        "recent_context".to_owned(),
    ];
    let summary = format!(
        "{MARKER}\nGeneration: {generation}\nCurrent goal: {}\nFocus for the next turn: {focus}\n\nActive plan (JSON):\n{plan}\n\nPending question and approval state (JSON):\n{pending_question}\n\nApproval grants (JSON):\n{approval_grants}\n\nApproval receipts (JSON):\n{approval_receipts}\n\nVerification evidence (JSON):\n{evidence}\n\nTool artifacts and edited-file references (JSON):\n{artifacts}\n\nRecent uncompacted context:\n{recent_context}",
        session.objective,
    );
    session.messages = vec![Message {
        role: Role::User,
        content: vec![MessageBlock::Text { text: summary }],
    }];
    append_event(
        session,
        Actor::Coordinator,
        EventPayload::ConversationCompacted {
            original_messages: u32::try_from(original_messages).unwrap_or(u32::MAX),
            retained_messages: 1,
            generation,
            source_event_sequences,
            preserved_sections,
        },
    )?;
    session.updated_at = OffsetDateTime::now_utc();
    persist(session)
}
'''
support.write_text(text[:start] + replacement + text[end:])

Path("crates/medusa-agent/tests/structured_compaction.rs").write_text(r'''use medusa_agent::{AgentEngine, AgentPlanStep, AgentPlanStepStatus};
use medusa_config::Config;
use medusa_core::MedusaResult;
use medusa_protocol::EventPayload;
use medusa_provider::{MessageBlock, ModelProvider, ModelRequest, ModelResponse};

struct IdleProvider;
impl ModelProvider for IdleProvider {
    fn complete(&self, _: &ModelRequest) -> MedusaResult<ModelResponse> {
        unreachable!("compaction does not call the provider")
    }
}

#[test]
fn structured_compaction_preserves_state_and_provenance() {
    let directory = tempfile::tempdir().expect("temporary repository");
    let engine = AgentEngine::new(IdleProvider, Config::default());
    let mut session = engine.create_session(directory.path(), "preserve migration".to_owned()).expect("session");
    session.plan = vec![AgentPlanStep {
        title: "Run cargo test -p medusa-agent".to_owned(),
        status: AgentPlanStepStatus::InProgress,
    }];
    session.evidence.push("migration failed at src/store.rs:42".to_owned());
    session.tool_artifacts.push(directory.path().join("src/store.rs"));
    engine.append_user_message(&mut session, vec![MessageBlock::Text {
        text: "Do not rename SessionId; keep the exact symbol.".to_owned(),
    }]).expect("append correction");
    let source_sequences = session.events.iter().map(|event| event.sequence).collect::<Vec<_>>();

    engine.compact_session(&mut session, Some("finish migration safely")).expect("compact");
    let MessageBlock::Text { text } = &session.messages[0].content[0] else { panic!("summary") };
    assert!(text.contains("Run cargo test -p medusa-agent"));
    assert!(text.contains("migration failed at src/store.rs:42"));
    assert!(text.contains("Do not rename SessionId; keep the exact symbol."));
    assert!(session.events.iter().any(|event| matches!(
        &event.payload,
        EventPayload::ConversationCompacted { generation: 1, source_event_sequences, preserved_sections, .. }
            if source_event_sequences == &source_sequences
                && preserved_sections.contains(&"verification_evidence".to_owned())
    )));
}

#[test]
fn repeated_compaction_does_not_nest_prior_summary() {
    let directory = tempfile::tempdir().expect("temporary repository");
    let engine = AgentEngine::new(IdleProvider, Config::default());
    let mut session = engine.create_session(directory.path(), "stable summary".to_owned()).expect("session");
    engine.compact_session(&mut session, None).expect("first");
    engine.compact_session(&mut session, None).expect("second");
    let MessageBlock::Text { text } = &session.messages[0].content[0] else { panic!("summary") };
    assert_eq!(text.matches("[medusa-compaction-v1]").count(), 1);
    assert!(text.contains("Generation: 2"));
}
''')
