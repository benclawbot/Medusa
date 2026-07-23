use medusa_agent::{AgentEngine, AgentPlanStep, AgentPlanStepStatus};
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
    let mut session = engine
        .create_session(directory.path(), "preserve migration".to_owned())
        .expect("session");
    session.plan = vec![AgentPlanStep {
        title: "Run cargo test -p medusa-agent".to_owned(),
        status: AgentPlanStepStatus::InProgress,
    }];
    session
        .evidence
        .push("migration failed at src/store.rs:42".to_owned());
    session
        .tool_artifacts
        .push(directory.path().join("src/store.rs"));
    engine
        .append_user_message(
            &mut session,
            vec![MessageBlock::Text {
                text: "Do not rename SessionId; keep the exact symbol.".to_owned(),
            }],
        )
        .expect("append correction");
    let source_sequences = session
        .events
        .iter()
        .map(|event| event.sequence)
        .collect::<Vec<_>>();

    engine
        .compact_session(&mut session, Some("finish migration safely"))
        .expect("compact");
    let MessageBlock::Text { text } = &session.messages[0].content[0] else {
        panic!("summary")
    };
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
    let mut session = engine
        .create_session(directory.path(), "stable summary".to_owned())
        .expect("session");
    engine.compact_session(&mut session, None).expect("first");
    engine.compact_session(&mut session, None).expect("second");
    let MessageBlock::Text { text } = &session.messages[0].content[0] else {
        panic!("summary")
    };
    assert_eq!(text.matches("[medusa-compaction-v1]").count(), 1);
    assert!(text.contains("Generation: 2"));
}
