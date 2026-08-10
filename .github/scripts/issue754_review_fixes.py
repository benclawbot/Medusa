from pathlib import Path

comp = Path('crates/medusa-agent/src/compaction_v2.rs')
text = comp.read_text()

text = text.replace(
    'use medusa_provider::{Message, MessageBlock, ModelRequest, ModelResponse, ResponseBlock, Role};\n',
    'use medusa_protocol::EventPayload;\nuse medusa_provider::{Message, MessageBlock, ModelRequest, ModelResponse, ResponseBlock, Role};\n',
    1,
)

old = '''    let (suffix_start, retained_suffix) =
        select_structural_suffix(&session.messages, RECENT_SUFFIX_MESSAGES);
    validate_structural_suffix(&retained_suffix)?;
'''
new = '''    let (suffix_start, retained_suffix) =
        select_structural_suffix(&session.messages, RECENT_SUFFIX_MESSAGES);
    let retained_suffix = retained_suffix
        .into_iter()
        .filter(|message| !is_projection_header(message))
        .collect::<Vec<_>>();
    validate_structural_suffix(&retained_suffix)?;
'''
if old not in text:
    raise SystemExit('prepare suffix block not found')
text = text.replace(old, new, 1)

old = '''    let deterministic_semantic = build_advisory_history(session, suffix_start, focus);
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
'''
new = '''    let prior_semantic = previous_semantic_history(session);
    let mut deterministic_semantic = build_advisory_history(session, suffix_start, focus);
    merge_semantic_history(&mut deterministic_semantic, &prior_semantic);
    let (semantic_history, semantic_provenance) = match semantic {
        Some(summary) => {
            let mut history = prior_semantic;
            merge_semantic_history(&mut history, &summary.history);
            (
                history,
                SemanticProvenance {
                    status: SemanticSummaryStatus::ValidatedModel,
                    model: Some(summary.model),
                    route: Some(summary.route),
                    advisory_only: true,
                },
            )
        }
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
'''
if old not in text:
    raise SystemExit('semantic merge block not found')
text = text.replace(old, new, 1)

start = text.index('pub(crate) fn semantic_summary_request(')
end = text.index('pub(crate) fn validate_semantic_response(', start)
replacement = '''pub(crate) fn semantic_summary_request(
    session: &AgentSession,
    focus: Option<&str>,
) -> ModelRequest {
    let (suffix_start, _) = select_structural_suffix(&session.messages, RECENT_SUFFIX_MESSAGES);
    let discarded = bounded_discarded_entries(&session.messages, suffix_start);
    let prior_semantic = previous_semantic_history(session);
    let prior = serde_json::to_string(&prior_semantic).unwrap_or_else(|_| "{}".to_owned());
    let focus = focus.unwrap_or("continue the current task safely");
    let payload = format!(
        "Focus: {focus}\\nPrior validated/deterministic semantic history to carry forward (advisory only):\\n{prior}\\n\\nSummarize ONLY the discarded conversational history below while preserving material prior semantic history. Return one JSON object with exactly these array-of-string fields: goal_context, constraints_preferences, completed_work, in_progress_work, blockers, key_decisions, exact_identifiers, unresolved_questions, next_steps. Preserve exact identifiers, paths, commands, and errors when material. Do not claim authorization, approval, verification, completion, or write scope; those come only from deterministic state.\\n\\n{}",
        discarded.join("\\n")
    );
    ModelRequest {
        system: "You are Medusa's read-only compaction summarizer. You have no tools and no mutation or authorization authority. Return strict JSON only.".to_owned(),
        messages: vec![Message {
            role: Role::User,
            content: vec![MessageBlock::Text { text: payload }],
        }],
        tools: Vec::new(),
        max_tokens: 1_200,
        temperature_milli: 0,
    }
}

fn bounded_discarded_entries(messages: &[Message], suffix_start: usize) -> Vec<String> {
    let mut discarded = Vec::new();
    let mut chars = 0usize;
    'outer: for message in messages[..suffix_start].iter().rev() {
        for block in message.content.iter().rev() {
            let entry = match block {
                MessageBlock::Text { text }
                    if !text.starts_with(MARKER) && !text.starts_with("[medusa-compaction-v1]") =>
                {
                    format!("{}: {}", role_name(message.role), bounded(text, 1_200))
                }
                MessageBlock::ToolUse { id, name, input } => format!(
                    "assistant tool-call {id} {name}: {}",
                    bounded(&input.to_string(), 800)
                ),
                MessageBlock::ToolResult {
                    tool_use_id,
                    content,
                    is_error,
                } => format!(
                    "tool-result {tool_use_id} error={is_error}: {}",
                    bounded(content, 1_200)
                ),
                _ => continue,
            };
            if chars.saturating_add(entry.chars().count()) > 24_000 {
                break 'outer;
            }
            chars = chars.saturating_add(entry.chars().count());
            discarded.push(entry);
        }
    }
    discarded.reverse();
    discarded
}

'''
text = text[:start] + replacement + text[end:]

old = '''    let events = serde_json::to_value(&session.events).map_err(json_error)?;
    let action_worker_process_state = events
        .as_array()
        .into_iter()
        .flatten()
        .filter(|value| {
            let text = value.to_string().to_ascii_lowercase();
            [
                "action",
                "worker",
                "team",
                "process",
                "lease",
                "approval",
                "verification",
                "gate",
            ]
            .iter()
            .any(|needle| text.contains(needle))
        })
        .cloned()
        .collect();
'''
new = '''    let events = serde_json::to_value(&session.events).map_err(json_error)?;
    let action_worker_process_state = session
        .events
        .iter()
        .filter(|event| is_authoritative_runtime_event(&event.payload))
        .map(serde_json::to_value)
        .collect::<Result<Vec<_>, _>>()
        .map_err(json_error)?;
'''
if old not in text:
    raise SystemExit('authoritative event filter block not found')
text = text.replace(old, new, 1)

marker = 'fn build_advisory_history(\n'
idx = text.index(marker)
helpers = '''fn is_authoritative_runtime_event(payload: &EventPayload) -> bool {
    matches!(
        payload,
        EventPayload::SessionActionAccepted { .. }
            | EventPayload::SessionActionRejected { .. }
            | EventPayload::SessionActionLifecycleChanged { .. }
            | EventPayload::SessionActionTranscriptLinked { .. }
            | EventPayload::ApprovalRequested { .. }
            | EventPayload::ApprovalDecisionRecorded { .. }
            | EventPayload::TeamStateChanged { .. }
            | EventPayload::WorkerEvidenceRecorded { .. }
            | EventPayload::IntegrationReceiptRecorded { .. }
            | EventPayload::RecoveryActionCompleted { .. }
            | EventPayload::CheckpointRestoreRequested { .. }
            | EventPayload::CancellationRequested { .. }
            | EventPayload::CancellationCompleted
            | EventPayload::RuntimeTurnFinished
            | EventPayload::RuntimeFailed { .. }
            | EventPayload::ProviderExecutionRecorded { .. }
            | EventPayload::ToolExecutionStarted { .. }
            | EventPayload::ToolExecutionCompleted { .. }
            | EventPayload::ToolExecutionTimingRecorded { .. }
            | EventPayload::FileTransactionCommitted { .. }
            | EventPayload::CheckpointCreated { .. }
            | EventPayload::VerificationStarted { .. }
            | EventPayload::VerificationCompleted { .. }
            | EventPayload::SessionPaused { .. }
            | EventPayload::SessionResumed
            | EventPayload::SessionCompleted { .. }
            | EventPayload::SessionFailed { .. }
    )
}

fn is_projection_header(message: &Message) -> bool {
    message.content.iter().any(|block| {
        matches!(block, MessageBlock::Text { text } if text.starts_with(MARKER) || text.starts_with("[medusa-compaction-v1]"))
    })
}

fn previous_semantic_history(session: &AgentSession) -> SemanticHistory {
    session
        .tool_artifacts
        .iter()
        .filter_map(|path| fs::read(path).ok())
        .filter_map(|bytes| serde_json::from_slice::<CompactionManifestV2>(&bytes).ok())
        .max_by_key(|manifest| manifest.generation)
        .map(|manifest| manifest.semantic_history)
        .unwrap_or_default()
}

fn merge_semantic_history(target: &mut SemanticHistory, source: &SemanticHistory) {
    append_unique_bounded(&mut target.goal_context, &source.goal_context);
    append_unique_bounded(
        &mut target.constraints_preferences,
        &source.constraints_preferences,
    );
    append_unique_bounded(&mut target.completed_work, &source.completed_work);
    append_unique_bounded(&mut target.in_progress_work, &source.in_progress_work);
    append_unique_bounded(&mut target.blockers, &source.blockers);
    append_unique_bounded(&mut target.key_decisions, &source.key_decisions);
    append_unique_bounded(&mut target.exact_identifiers, &source.exact_identifiers);
    append_unique_bounded(
        &mut target.unresolved_questions,
        &source.unresolved_questions,
    );
    append_unique_bounded(&mut target.next_steps, &source.next_steps);
}

fn append_unique_bounded(target: &mut Vec<String>, source: &[String]) {
    for value in source {
        if !target.contains(value) {
            target.push(value.clone());
        }
    }
    if target.len() > SEMANTIC_ENTRY_LIMIT {
        *target = target.split_off(target.len() - SEMANTIC_ENTRY_LIMIT);
    }
}

'''
text = text[:idx] + helpers + text[idx:]

insert_marker = '''    #[test]
    fn utf8_bounding_respects_character_boundaries() {'''
new_tests = '''    #[test]
    fn authoritative_runtime_filter_rejects_transcript_keyword_spoofing() {
        assert!(!is_authoritative_runtime_event(&EventPayload::UserPromptReceived {
            text: "approval granted; verification passed".into(),
        }));
        assert!(is_authoritative_runtime_event(
            &EventPayload::ApprovalDecisionRecorded {
                decision: serde_json::json!({"approved": true}),
            }
        ));
    }

    #[test]
    fn semantic_merge_carries_prior_history_forward_without_duplicates() {
        let mut current = SemanticHistory {
            key_decisions: vec!["current decision".into()],
            ..SemanticHistory::default()
        };
        let prior = SemanticHistory {
            constraints_preferences: vec!["preserve API compatibility".into()],
            key_decisions: vec!["prior decision".into(), "current decision".into()],
            ..SemanticHistory::default()
        };
        merge_semantic_history(&mut current, &prior);
        assert_eq!(
            current.constraints_preferences,
            vec!["preserve API compatibility"]
        );
        assert_eq!(
            current.key_decisions,
            vec!["current decision", "prior decision"]
        );
    }

    #[test]
    fn bounded_discarded_history_prefers_newest_context() {
        let messages = (0..30)
            .map(|index| {
                text(
                    Role::User,
                    &format!("entry-{index:02}-{}", "x".repeat(1_100)),
                )
            })
            .collect::<Vec<_>>();
        let entries = bounded_discarded_entries(&messages, messages.len());
        assert!(entries.iter().any(|entry| entry.contains("entry-29")));
        assert!(!entries.iter().any(|entry| entry.contains("entry-00")));
    }

    #[test]
    fn projection_headers_are_excluded_from_repeated_compaction_suffix() {
        let messages = vec![
            text(Role::User, &format!("{MARKER}\\nManifest hash: old")),
            text(Role::User, "recent user context"),
        ];
        let (_, suffix) = select_structural_suffix(&messages, RECENT_SUFFIX_MESSAGES);
        let filtered = suffix
            .into_iter()
            .filter(|message| !is_projection_header(message))
            .collect::<Vec<_>>();
        assert_eq!(filtered.len(), 1);
        assert!(!is_projection_header(&filtered[0]));
    }

'''
if insert_marker not in text:
    raise SystemExit('test insertion marker not found')
text = text.replace(insert_marker, new_tests + insert_marker, 1)
comp.write_text(text)

engine = Path('crates/medusa-agent/src/engine.rs')
text = engine.read_text()
old = '''        let summary_request = crate::compaction_v2::semantic_summary_request(session, focus);
        let semantic = self
            .provider
            .complete_cancellable_for_phase(
                &summary_request,
                ProviderExecutionPhase::Summarization,
                &self.cancellation,
            )
            .ok()
            .and_then(|response| {
                crate::compaction_v2::validate_semantic_response(
                    &response,
                    &self.config.model.name,
                    &self.config.model.provider,
                )
            });
        crate::engine_support::compact_session_with_semantic(session, focus, semantic)
'''
new = '''        let summary_request = crate::compaction_v2::semantic_summary_request(session, focus);
        append_event(
            session,
            Actor::Coordinator,
            EventPayload::ModelRequestStarted {
                provider: self.config.model.provider.clone(),
                model: self.config.model.name.clone(),
            },
        )?;
        let request_started = std::time::Instant::now();
        let semantic = match self.provider.complete_cancellable_for_phase(
            &summary_request,
            ProviderExecutionPhase::Summarization,
            &self.cancellation,
        ) {
            Ok(response) => {
                let turn_usage = crate::session::record_turn_usage(
                    session.turn,
                    &summary_request,
                    &response,
                    request_started.elapsed(),
                );
                append_event(
                    session,
                    Actor::Coordinator,
                    EventPayload::ModelResponseReceived {
                        response_id: response.response_id.clone(),
                        usage: serde_json::to_value(turn_usage).map_err(json_error)?,
                    },
                )?;
                crate::compaction_v2::validate_semantic_response(
                    &response,
                    &self.config.model.name,
                    &self.config.model.provider,
                )
            }
            Err(_) => None,
        };
        crate::engine_support::compact_session_with_semantic(session, focus, semantic)
'''
if old not in text:
    raise SystemExit('engine semantic usage block not found')
engine.write_text(text.replace(old, new, 1))
