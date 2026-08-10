from pathlib import Path

p = Path("crates/medusa-agent/src/compaction_v2.rs")
text = p.read_text()
text = text.replace(
    "use medusa_provider::{Message, MessageBlock, Role};",
    "use medusa_provider::{Message, MessageBlock, ModelRequest, ModelResponse, ResponseBlock, Role};",
    1,
)
if "pub(crate) struct ValidatedSemanticSummary" not in text:
    text = text.replace(
        "pub(crate) struct PreparedCompaction {",
        """pub(crate) struct ValidatedSemanticSummary {
    pub history: SemanticHistory,
    pub model: String,
    pub route: String,
}

pub(crate) struct PreparedCompaction {""",
        1,
    )
old_sig = "pub(crate) fn prepare(session: &AgentSession, focus: Option<&str>) -> MedusaResult<PreparedCompaction> {"
if old_sig in text:
    text = text.replace(
        old_sig,
        """pub(crate) fn prepare(session: &AgentSession, focus: Option<&str>) -> MedusaResult<PreparedCompaction> {
    prepare_with_semantic(session, focus, None)
}

pub(crate) fn prepare_with_semantic(
    session: &AgentSession,
    focus: Option<&str>,
    semantic: Option<ValidatedSemanticSummary>,
) -> MedusaResult<PreparedCompaction> {""",
        1,
    )
old = "    let semantic_history = build_advisory_history(session, suffix_start, focus);\n"
if old in text:
    text = text.replace(
        old,
        """    let deterministic_semantic = build_advisory_history(session, suffix_start, focus);
    let (semantic_history, semantic_provenance) = match semantic {
        Some(summary) => (
            summary.history,
            SemanticProvenance {
                status: SemanticSummaryStatus::ValidatedModel,
                model: Some(summary.model),
                route: Some(summary.route),
                advisory_only: true,
            },
        ),
        None => (
            deterministic_semantic,
            SemanticProvenance {
                status: SemanticSummaryStatus::DeterministicFallback,
                model: None,
                route: None,
                advisory_only: true,
            },
        ),
    };
""",
        1,
    )
old_prov = """        semantic_provenance: SemanticProvenance {
            status: SemanticSummaryStatus::DeterministicFallback,
            model: None,
            route: None,
            advisory_only: true,
        },"""
text = text.replace(old_prov, "        semantic_provenance,", 1)
text = text.replace(
    '        "semantic_summary_status=deterministic_fallback".to_owned(),',
    '        format!("semantic_summary_status={:?}", manifest.semantic_provenance.status).to_ascii_lowercase(),',
    1,
)
if "pub(crate) fn semantic_summary_request" not in text:
    needle = "fn build_authoritative(session: &AgentSession) -> MedusaResult<AuthoritativeState> {"
    funcs = r'''pub(crate) fn semantic_summary_request(session: &AgentSession, focus: Option<&str>) -> ModelRequest {
    let (suffix_start, _) = select_structural_suffix(&session.messages, RECENT_SUFFIX_MESSAGES);
    let mut discarded = Vec::new();
    let mut chars = 0usize;
    'outer: for message in &session.messages[..suffix_start] {
        for block in &message.content {
            let entry = match block {
                MessageBlock::Text { text } if !text.starts_with(MARKER) && !text.starts_with("[medusa-compaction-v1]") => format!("{}: {}", role_name(message.role), bounded(text, 1_200)),
                MessageBlock::ToolUse { id, name, input } => format!("assistant tool-call {id} {name}: {}", bounded(&input.to_string(), 800)),
                MessageBlock::ToolResult { tool_use_id, content, is_error } => format!("tool-result {tool_use_id} error={is_error}: {}", bounded(content, 1_200)),
                _ => continue,
            };
            if chars.saturating_add(entry.chars().count()) > 24_000 { break 'outer; }
            chars = chars.saturating_add(entry.chars().count());
            discarded.push(entry);
        }
    }
    let focus = focus.unwrap_or("continue the current task safely");
    let payload = format!("Focus: {focus}\nSummarize ONLY the discarded conversational history below. Return one JSON object with exactly these array-of-string fields: goal_context, constraints_preferences, completed_work, in_progress_work, blockers, key_decisions, exact_identifiers, unresolved_questions, next_steps. Preserve exact identifiers, paths, commands, and errors when material. Do not claim authorization, approval, verification, completion, or write scope; those come only from deterministic state.\n\n{}", discarded.join("\n"));
    ModelRequest { system: "You are Medusa's read-only compaction summarizer. You have no tools and no mutation or authorization authority. Return strict JSON only.".to_owned(), messages: vec![Message { role: Role::User, content: vec![MessageBlock::Text { text: payload }] }], tools: Vec::new(), max_tokens: 1_200, temperature_milli: 0 }
}

pub(crate) fn validate_semantic_response(response: &ModelResponse, model: &str, route: &str) -> Option<ValidatedSemanticSummary> {
    if response.blocks.len() != 1 { return None; }
    let ResponseBlock::Text { text } = &response.blocks[0] else { return None; };
    let clean = text.trim().strip_prefix("```json").unwrap_or(text.trim());
    let clean = clean.strip_suffix("```").unwrap_or(clean).trim();
    let history: SemanticHistory = serde_json::from_str(clean).ok()?;
    let arrays = [&history.goal_context, &history.constraints_preferences, &history.completed_work, &history.in_progress_work, &history.blockers, &history.key_decisions, &history.exact_identifiers, &history.unresolved_questions, &history.next_steps];
    let entries = arrays.iter().map(|values| values.len()).sum::<usize>();
    let chars = arrays.iter().flat_map(|values| values.iter()).map(|value| value.chars().count()).sum::<usize>();
    if entries > 96 || chars > 32_000 || arrays.iter().flat_map(|values| values.iter()).any(|value| value.chars().count() > 2_000) { return None; }
    Some(ValidatedSemanticSummary { history, model: model.to_owned(), route: route.to_owned() })
}

'''
    text = text.replace(needle, funcs + needle, 1)
if "            MessageBlock::Text { .. } => None," not in text:
    text = text.replace("            MessageBlock::Image { .. } => None,", "            MessageBlock::Text { .. } => None,\n            MessageBlock::Image { .. } => None,", 1)
p.write_text(text)

lib = Path("crates/medusa-agent/src/lib.rs")
t = lib.read_text()
if "pub mod compaction_v2;" not in t:
    t = t.replace("mod approval;\n", "mod approval;\npub mod compaction_v2;\n", 1)
lib.write_text(t)

support = Path("crates/medusa-agent/src/engine_support.rs")
t = support.read_text()
t = t.replace("ImageSource, Message, MessageBlock, ProviderCapabilities, ProviderExecutionPhase, Role,\n", "ImageSource, Message, MessageBlock, ProviderCapabilities, ProviderExecutionPhase,\n", 1)
if "without requiring a live model provider" in t:
    start = t.index("/// Compacts durable session history without requiring a live model provider.\npub fn compact_session")
    end = t.index("pub(crate) fn compact_message_text", start)
    replacement = '''/// Compacts durable session history into a crash-safe V2 hybrid manifest.
pub fn compact_session(session: &mut AgentSession, focus: Option<&str>) -> MedusaResult<()> {
    compact_session_with_semantic(session, focus, None)
}

pub(crate) fn compact_session_with_semantic(session: &mut AgentSession, focus: Option<&str>, semantic: Option<crate::compaction_v2::ValidatedSemanticSummary>) -> MedusaResult<()> {
    let original_messages = session.messages.len();
    let source_event_sequences = session.events.iter().map(|event| event.sequence).collect::<Vec<_>>();
    let migrated_v1 = session.messages.iter().flat_map(|message| &message.content).any(|block| matches!(block, MessageBlock::Text { text } if text.starts_with("[medusa-compaction-v1]")));
    let prepared = crate::compaction_v2::prepare_with_semantic(session, focus, semantic)?;
    let generation = prepared.manifest.generation;
    let mut preserved_sections = prepared.preserved_sections;
    preserved_sections.push(format!("migrated_v1={migrated_v1}"));
    if !session.tool_artifacts.contains(&prepared.manifest_path) { session.tool_artifacts.push(prepared.manifest_path.clone()); }
    session.messages = prepared.projection;
    append_event(session, Actor::Coordinator, EventPayload::ConversationCompacted { original_messages: u32::try_from(original_messages).unwrap_or(u32::MAX), retained_messages: u32::try_from(session.messages.len()).unwrap_or(u32::MAX), generation, source_event_sequences, preserved_sections })?;
    session.updated_at = OffsetDateTime::now_utc();
    persist(session)
}

'''
    t = t[:start] + replacement + t[end:]
support.write_text(t)

engine = Path("crates/medusa-agent/src/engine.rs")
t = engine.read_text()
# Keep the explicit/manual compact_session API provider-free for deterministic tooling/tests.
if "    fn compact_session_v2(&self" not in t:
    marker = "    pub fn run_to_completion(&self, session: &mut AgentSession) -> MedusaResult<()> {"
    helper = '''    fn compact_session_v2(&self, session: &mut AgentSession, focus: Option<&str>) -> MedusaResult<()> {
        let summary_request = crate::compaction_v2::semantic_summary_request(session, focus);
        let semantic = self.provider.complete_cancellable_for_phase(&summary_request, ProviderExecutionPhase::Summarization, &self.cancellation).ok().and_then(|response| crate::compaction_v2::validate_semantic_response(&response, &self.config.model.name, &self.config.model.provider));
        crate::engine_support::compact_session_with_semantic(session, focus, semantic)
    }

'''
    t = t.replace(marker, helper + marker, 1)
t = t.replace('            compact_session(\n                session,\n                Some("preserve the current objective, decisions, tool results, and pending work"),\n            )?;', '            self.compact_session_v2(\n                session,\n                Some("preserve the current objective, decisions, tool results, and pending work"),\n            )?;', 1)
t = t.replace('                    compact_session(\n                        session,', '                    self.compact_session_v2(\n                        session,', 1)
engine.write_text(t)
