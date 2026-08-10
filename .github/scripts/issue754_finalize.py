from pathlib import Path

p = Path('crates/medusa-agent/src/compaction_v2.rs')
text = p.read_text()
unused = '''pub(crate) fn prepare(
    session: &AgentSession,
    focus: Option<&str>,
) -> MedusaResult<PreparedCompaction> {
    prepare_with_semantic(session, focus, None)
}

'''
text = text.replace(unused, '', 1)
if 'fn malformed_semantic_response_falls_back_safely()' not in text:
    marker = '''    #[test]
    fn utf8_bounding_respects_character_boundaries() {'''
    tests = '''    #[test]
    fn malformed_semantic_response_falls_back_safely() {
        let response = ModelResponse {
            response_id: None,
            stop_reason: None,
            blocks: vec![ResponseBlock::Text { text: "not-json".into() }],
            usage: Default::default(),
        };
        assert!(validate_semantic_response(&response, "model", "route").is_none());
    }

    #[test]
    fn validated_semantic_response_records_provenance_without_authority() {
        let response = ModelResponse {
            response_id: None,
            stop_reason: None,
            blocks: vec![ResponseBlock::Text {
                text: r#"{"goal_context":["continue"],"constraints_preferences":[],"completed_work":[],"in_progress_work":[],"blockers":[],"key_decisions":[],"exact_identifiers":["src/lib.rs"],"unresolved_questions":[],"next_steps":[]}"#.into(),
            }],
            usage: Default::default(),
        };
        let summary = validate_semantic_response(&response, "model-x", "route-y").expect("valid summary");
        assert_eq!(summary.model, "model-x");
        assert_eq!(summary.route, "route-y");
        assert_eq!(summary.history.exact_identifiers, vec!["src/lib.rs"]);
    }

'''
    text = text.replace(marker, tests + marker, 1)
p.write_text(text)
