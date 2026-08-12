use std::collections::BTreeMap;

use medusa_protocol::frontend::{
    ApprovalDecision, FrontendEvent, FrontendEventEnvelope, PresentationActivityKind,
    PresentationLifecycle,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use time::{Duration, OffsetDateTime};

use super::{
    TelegramDisplayConfig, TelegramGatewayError, ToolProgressMode, split_telegram_text,
    telegram_markdown_v2,
};

const TELEGRAM_TEXT_LIMIT_UTF16: usize = 4_000;
const MAX_REPLAY_RECORDS: usize = 4_096;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TelegramReaction {
    Processing,
    Success,
    Failure,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TelegramParseMode {
    Plain,
    MarkdownV2,
}

#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(tag = "type", content = "id", rename_all = "snake_case")]
pub enum TelegramMessageSlot {
    Preview(u16),
    Progress,
    Plan,
    Team,
    Question(String),
    Approval(String),
    Interim(String),
    Notice(String),
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum TelegramButtonIntent {
    AnswerQuestion {
        question_id: String,
        value: String,
    },
    Approval {
        approval_id: String,
        decision: ApprovalDecision,
    },
    Details {
        reference: String,
    },
    CancelQueued,
    StartLiveVoice,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TelegramRenderButton {
    pub label: String,
    pub intent: TelegramButtonIntent,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum TelegramAction {
    SetReaction {
        reaction: Option<TelegramReaction>,
    },
    SetTyping {
        active: bool,
    },
    UpsertText {
        slot: TelegramMessageSlot,
        text: String,
        parse_mode: TelegramParseMode,
        buttons: Vec<TelegramRenderButton>,
        disable_link_preview: bool,
    },
    DeleteSlot {
        slot: TelegramMessageSlot,
    },
    SendArtifact {
        artifact_id: String,
        evidence_ref: String,
        caption: Option<String>,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct RenderedEvent {
    event_id: String,
    fingerprint: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TelegramRenderer {
    config: TelegramDisplayConfig,
    source_message_id: i64,
    preview: String,
    preview_chunk_count: usize,
    last_edit_at: Option<OffsetDateTime>,
    last_flushed_chars: usize,
    cursor_events: BTreeMap<u64, RenderedEvent>,
    active: bool,
}

impl TelegramRenderer {
    pub fn new(
        config: TelegramDisplayConfig,
        source_message_id: i64,
    ) -> Result<Self, TelegramGatewayError> {
        config.validate()?;
        Ok(Self {
            config,
            source_message_id,
            preview: String::new(),
            preview_chunk_count: 0,
            last_edit_at: None,
            last_flushed_chars: 0,
            cursor_events: BTreeMap::new(),
            active: false,
        })
    }

    #[must_use]
    pub const fn source_message_id(&self) -> i64 {
        self.source_message_id
    }

    #[must_use]
    pub const fn is_active(&self) -> bool {
        self.active
    }

    /// Starts a new user-visible turn while retaining replay fingerprints.
    pub fn begin_turn(&mut self, source_message_id: i64) {
        self.source_message_id = source_message_id;
        self.preview.clear();
        self.preview_chunk_count = 0;
        self.last_edit_at = None;
        self.last_flushed_chars = 0;
        self.active = false;
    }

    pub fn render(
        &mut self,
        envelope: &FrontendEventEnvelope,
        now: OffsetDateTime,
    ) -> Result<Vec<TelegramAction>, TelegramGatewayError> {
        envelope
            .validate()
            .map_err(|error| TelegramGatewayError::Protocol(error.to_owned()))?;
        let fingerprint = event_fingerprint(envelope)?;
        if self.already_rendered(envelope, &fingerprint)? {
            return Ok(Vec::new());
        }

        let mut actions = Vec::new();
        match &envelope.event {
            FrontendEvent::SubmissionAccepted | FrontendEvent::Started => {
                self.active = true;
                actions.push(TelegramAction::SetReaction {
                    reaction: Some(TelegramReaction::Processing),
                });
                actions.push(TelegramAction::SetTyping { active: true });
            }
            FrontendEvent::SubmissionQueued { position } => {
                actions.push(self.text_action(
                    TelegramMessageSlot::Progress,
                    format!("Queued — position {position}"),
                    TelegramParseMode::Plain,
                    vec![TelegramRenderButton {
                        label: "Cancel queued".to_owned(),
                        intent: TelegramButtonIntent::CancelQueued,
                    }],
                ));
            }
            FrontendEvent::AssistantTextDelta { text } => {
                let previous_len = self.preview.len();
                self.preview.push_str(text);
                if self.should_flush(now) {
                    match self.flush_preview(true, now) {
                        Ok(flushed) => actions.extend(flushed),
                        Err(error) => {
                            self.preview.truncate(previous_len);
                            return Err(error);
                        }
                    }
                }
            }
            FrontendEvent::AssistantInterim { text } if self.config.interim_assistant_messages => {
                actions.push(self.text_action(
                    TelegramMessageSlot::Interim(envelope.event_id.clone()),
                    telegram_markdown_v2(text),
                    TelegramParseMode::MarkdownV2,
                    Vec::new(),
                ));
            }
            FrontendEvent::Activity(activity) => {
                if let Some(text) = render_activity(
                    activity.kind,
                    activity.lifecycle,
                    &activity.title,
                    &activity.details,
                    self.config.tool_progress,
                ) {
                    let buttons = activity
                        .evidence_ref
                        .as_ref()
                        .map(|reference| {
                            vec![TelegramRenderButton {
                                label: "Details".to_owned(),
                                intent: TelegramButtonIntent::Details {
                                    reference: reference.clone(),
                                },
                            }]
                        })
                        .unwrap_or_default();
                    actions.push(self.text_action(
                        TelegramMessageSlot::Progress,
                        text,
                        TelegramParseMode::Plain,
                        buttons,
                    ));
                }
            }
            FrontendEvent::Plan { steps, current } => {
                let mut text = String::from("Plan\n\n");
                for step in steps {
                    text.push_str(lifecycle_icon(step.lifecycle));
                    text.push(' ');
                    text.push_str(&step.title);
                    text.push('\n');
                }
                if let Some(current) = current {
                    text.push_str("\nCurrent: ");
                    text.push_str(current);
                }
                actions.push(self.text_action(
                    TelegramMessageSlot::Plan,
                    text,
                    TelegramParseMode::Plain,
                    Vec::new(),
                ));
            }
            FrontendEvent::Team {
                workers,
                verification,
            } => {
                let mut text = String::from("Team\n\n");
                for worker in workers {
                    text.push_str(lifecycle_icon(worker.lifecycle));
                    text.push(' ');
                    text.push_str(&worker.role);
                    text.push_str(" — ");
                    text.push_str(&worker.task);
                    if self.config.busy_detail {
                        if let Some(action) = &worker.current_action {
                            text.push_str(" — ");
                            text.push_str(action);
                        }
                    }
                    text.push('\n');
                }
                if let Some(verification) = verification {
                    text.push_str("\nVerification: ");
                    text.push_str(verification);
                }
                actions.push(self.text_action(
                    TelegramMessageSlot::Team,
                    text,
                    TelegramParseMode::Plain,
                    Vec::new(),
                ));
            }
            FrontendEvent::Question(question) => {
                let buttons = question
                    .options
                    .iter()
                    .map(|option| TelegramRenderButton {
                        label: option.label.clone(),
                        intent: TelegramButtonIntent::AnswerQuestion {
                            question_id: question.question_id.clone(),
                            value: option.value.clone(),
                        },
                    })
                    .collect();
                actions.push(self.text_action(
                    TelegramMessageSlot::Question(question.question_id.clone()),
                    question.prompt.clone(),
                    TelegramParseMode::Plain,
                    buttons,
                ));
            }
            FrontendEvent::ApprovalRequired(approval) => {
                let text = format!(
                    "Approval required\n\nAction: {}\nScope: {}\nReason: {}\nRisk: {}",
                    approval.action, approval.scope, approval.reason, approval.risk
                );
                let buttons = vec![
                    TelegramRenderButton {
                        label: "Approve once".to_owned(),
                        intent: TelegramButtonIntent::Approval {
                            approval_id: approval.approval_id.clone(),
                            decision: ApprovalDecision::ApproveOnce,
                        },
                    },
                    TelegramRenderButton {
                        label: "Deny".to_owned(),
                        intent: TelegramButtonIntent::Approval {
                            approval_id: approval.approval_id.clone(),
                            decision: ApprovalDecision::Deny,
                        },
                    },
                    TelegramRenderButton {
                        label: "Details".to_owned(),
                        intent: TelegramButtonIntent::Details {
                            reference: approval.approval_id.clone(),
                        },
                    },
                ];
                actions.push(self.text_action(
                    TelegramMessageSlot::Approval(approval.approval_id.clone()),
                    text,
                    TelegramParseMode::Plain,
                    buttons,
                ));
            }
            FrontendEvent::Progress { turn, phase } if self.config.long_running_notifications => {
                let suffix = phase
                    .as_ref()
                    .map(|phase| format!(" — {phase}"))
                    .unwrap_or_default();
                actions.push(self.text_action(
                    TelegramMessageSlot::Progress,
                    format!("⏳ Working — turn {turn}{suffix}"),
                    TelegramParseMode::Plain,
                    Vec::new(),
                ));
            }
            FrontendEvent::SettingsChanged {
                model,
                effort,
                plan_mode,
            } => {
                let plan = if *plan_mode { "on" } else { "off" };
                actions.push(self.text_action(
                    TelegramMessageSlot::Notice(envelope.event_id.clone()),
                    format!("Settings updated — model {model}, effort {effort}, plan {plan}"),
                    TelegramParseMode::Plain,
                    Vec::new(),
                ));
            }
            FrontendEvent::Notice {
                severity,
                title,
                details,
            } => {
                let mut text = format!("{}: {}", severity.to_uppercase(), title);
                if !details.is_empty() {
                    text.push('\n');
                    text.push_str(&details.join("\n"));
                }
                actions.push(self.text_action(
                    TelegramMessageSlot::Notice(envelope.event_id.clone()),
                    text,
                    TelegramParseMode::Plain,
                    Vec::new(),
                ));
            }
            FrontendEvent::Artifact(artifact) => {
                actions.push(TelegramAction::SendArtifact {
                    artifact_id: artifact.artifact_id.clone(),
                    evidence_ref: artifact.evidence_ref.clone(),
                    caption: artifact.caption.clone(),
                });
            }
            FrontendEvent::TurnFinished => {
                actions.extend(self.flush_preview(false, now)?);
                actions.push(TelegramAction::SetTyping { active: false });
            }
            FrontendEvent::Completed { summary } => {
                actions.extend(self.flush_preview(false, now)?);
                if self.preview.trim().is_empty() {
                    if let Some(summary) = summary {
                        actions.push(self.text_action(
                            TelegramMessageSlot::Preview(0),
                            telegram_markdown_v2(summary),
                            TelegramParseMode::MarkdownV2,
                            Vec::new(),
                        ));
                    }
                }
                actions.push(TelegramAction::SetTyping { active: false });
                actions.push(TelegramAction::SetReaction {
                    reaction: Some(TelegramReaction::Success),
                });
                if self.config.cleanup_progress {
                    actions.push(TelegramAction::DeleteSlot {
                        slot: TelegramMessageSlot::Progress,
                    });
                }
                self.active = false;
            }
            FrontendEvent::Cancelled { reason } => {
                actions.push(TelegramAction::SetTyping { active: false });
                actions.push(TelegramAction::SetReaction { reaction: None });
                let text = reason
                    .as_ref()
                    .map(|reason| format!("Cancelled — {reason}"))
                    .unwrap_or_else(|| "Cancelled".to_owned());
                actions.push(self.text_action(
                    TelegramMessageSlot::Progress,
                    text,
                    TelegramParseMode::Plain,
                    Vec::new(),
                ));
                self.active = false;
            }
            FrontendEvent::Failed { message, recovery } => {
                actions.extend(self.flush_preview(false, now)?);
                actions.push(TelegramAction::SetTyping { active: false });
                actions.push(TelegramAction::SetReaction {
                    reaction: Some(TelegramReaction::Failure),
                });
                let mut text = format!("Failed — {message}");
                if !recovery.is_empty() {
                    text.push_str("\n\nRecovery:\n");
                    for item in recovery {
                        text.push_str("• ");
                        text.push_str(item);
                        text.push('\n');
                    }
                }
                actions.push(self.text_action(
                    TelegramMessageSlot::Progress,
                    text,
                    TelegramParseMode::Plain,
                    Vec::new(),
                ));
                self.active = false;
            }
            FrontendEvent::Usage { .. }
            | FrontendEvent::AssistantInterim { .. }
            | FrontendEvent::Progress { .. } => {}
        }
        self.record_rendered(envelope, fingerprint);
        Ok(actions)
    }

    fn already_rendered(
        &self,
        envelope: &FrontendEventEnvelope,
        fingerprint: &str,
    ) -> Result<bool, TelegramGatewayError> {
        if let Some(existing) = self.cursor_events.get(&envelope.cursor) {
            if existing.event_id == envelope.event_id && existing.fingerprint == fingerprint {
                return Ok(true);
            }
            return Err(TelegramGatewayError::CursorConflict(envelope.cursor));
        }
        if self
            .cursor_events
            .last_key_value()
            .is_some_and(|(cursor, _)| envelope.cursor < *cursor)
        {
            return Err(TelegramGatewayError::StaleCursor(envelope.cursor));
        }
        Ok(false)
    }

    fn record_rendered(&mut self, envelope: &FrontendEventEnvelope, fingerprint: String) {
        self.cursor_events.insert(
            envelope.cursor,
            RenderedEvent {
                event_id: envelope.event_id.clone(),
                fingerprint,
            },
        );
        while self.cursor_events.len() > MAX_REPLAY_RECORDS {
            let Some(first) = self
                .cursor_events
                .first_key_value()
                .map(|(cursor, _)| *cursor)
            else {
                break;
            };
            self.cursor_events.remove(&first);
        }
    }

    fn should_flush(&self, now: OffsetDateTime) -> bool {
        if !self.config.streaming {
            return false;
        }
        let Some(last_edit_at) = self.last_edit_at else {
            return true;
        };
        let new_chars = self
            .preview
            .chars()
            .count()
            .saturating_sub(self.last_flushed_chars);
        let elapsed = now - last_edit_at;
        new_chars >= self.config.buffer_threshold_chars
            || elapsed >= Duration::milliseconds(self.config.edit_interval_ms as i64)
    }

    fn flush_preview(
        &mut self,
        streaming: bool,
        now: OffsetDateTime,
    ) -> Result<Vec<TelegramAction>, TelegramGatewayError> {
        if self.preview.is_empty() {
            return Ok(Vec::new());
        }
        let chunks = split_telegram_text(&self.preview, TELEGRAM_TEXT_LIMIT_UTF16);
        let mut actions = Vec::new();
        for (index, chunk) in chunks.iter().enumerate() {
            let slot = preview_slot(index)?;
            let final_chunk = index + 1 == chunks.len();
            let text = if streaming && final_chunk {
                format!("{chunk}{}", self.config.cursor)
            } else if streaming {
                chunk.clone()
            } else {
                telegram_markdown_v2(chunk)
            };
            let parse_mode = if streaming {
                TelegramParseMode::Plain
            } else {
                TelegramParseMode::MarkdownV2
            };
            actions.push(self.text_action(slot, text, parse_mode, Vec::new()));
        }
        for index in chunks.len()..self.preview_chunk_count {
            actions.push(TelegramAction::DeleteSlot {
                slot: preview_slot(index)?,
            });
        }
        self.preview_chunk_count = chunks.len();
        self.last_flushed_chars = self.preview.chars().count();
        self.last_edit_at = Some(now);
        Ok(actions)
    }

    fn text_action(
        &self,
        slot: TelegramMessageSlot,
        text: String,
        parse_mode: TelegramParseMode,
        buttons: Vec<TelegramRenderButton>,
    ) -> TelegramAction {
        TelegramAction::UpsertText {
            slot,
            text,
            parse_mode,
            buttons,
            disable_link_preview: self.config.disable_link_previews,
        }
    }
}

fn event_fingerprint(envelope: &FrontendEventEnvelope) -> Result<String, TelegramGatewayError> {
    let encoded = serde_json::to_vec(envelope)
        .map_err(|error| TelegramGatewayError::Protocol(error.to_string()))?;
    Ok(hex::encode(Sha256::digest(encoded)))
}

fn preview_slot(index: usize) -> Result<TelegramMessageSlot, TelegramGatewayError> {
    let index = u16::try_from(index).map_err(|_| TelegramGatewayError::TooManyMessageChunks)?;
    Ok(TelegramMessageSlot::Preview(index))
}

fn render_activity(
    kind: PresentationActivityKind,
    lifecycle: PresentationLifecycle,
    title: &str,
    details: &[String],
    mode: ToolProgressMode,
) -> Option<String> {
    if mode == ToolProgressMode::Off {
        return None;
    }
    if mode == ToolProgressMode::New
        && !matches!(
            lifecycle,
            PresentationLifecycle::Active | PresentationLifecycle::Waiting
        )
    {
        return None;
    }
    let icon = match kind {
        PresentationActivityKind::Assistant => "💬",
        PresentationActivityKind::RepositoryRead => "🔎",
        PresentationActivityKind::Edit => "✏️",
        PresentationActivityKind::Command => "💻",
        PresentationActivityKind::Test | PresentationActivityKind::Verification => "🧪",
        PresentationActivityKind::Approval => "🔐",
        PresentationActivityKind::Worker => "👥",
        PresentationActivityKind::Integration => "🔀",
        PresentationActivityKind::Recovery => "♻️",
        PresentationActivityKind::Progress => "⏳",
        PresentationActivityKind::Done => "✅",
        PresentationActivityKind::Error => "❌",
    };
    let mut text = format!("{icon} {title}");
    if mode == ToolProgressMode::Verbose && !details.is_empty() {
        text.push('\n');
        text.push_str(&details.join("\n"));
    } else if matches!(mode, ToolProgressMode::All | ToolProgressMode::Verbose)
        && lifecycle == PresentationLifecycle::Failed
        && !details.is_empty()
    {
        text.push_str(" — ");
        text.push_str(&details.join("; "));
    }
    Some(text)
}

fn lifecycle_icon(lifecycle: PresentationLifecycle) -> &'static str {
    match lifecycle {
        PresentationLifecycle::Active => "◉",
        PresentationLifecycle::Waiting | PresentationLifecycle::Informational => "○",
        PresentationLifecycle::Succeeded => "✓",
        PresentationLifecycle::Failed => "✕",
        PresentationLifecycle::Cancelled => "–",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use medusa_protocol::frontend::{
        FRONTEND_PROTOCOL_VERSION, FrontendEvent, PresentationActivity, PresentationApproval,
        PresentationLifecycle,
    };
    use time::macros::datetime;

    fn event(cursor: u64, event: FrontendEvent) -> FrontendEventEnvelope {
        FrontendEventEnvelope {
            protocol_version: FRONTEND_PROTOCOL_VERSION,
            event_id: format!("event-{cursor}"),
            cursor,
            session_id: "session-1".to_owned(),
            turn_id: Some("turn-1".to_owned()),
            parent_event_id: None,
            correlation_id: "correlation-1".to_owned(),
            timestamp: datetime!(2026-07-30 16:00 UTC),
            lifecycle: PresentationLifecycle::Active,
            event,
        }
    }

    #[test]
    fn renderer_replays_once_and_rejects_cursor_conflicts() {
        let mut renderer =
            TelegramRenderer::new(TelegramDisplayConfig::default(), 9).expect("renderer");
        let started = event(1, FrontendEvent::Started);
        assert_eq!(
            renderer
                .render(&started, started.timestamp)
                .expect("render")
                .len(),
            2
        );
        assert!(
            renderer
                .render(&started, started.timestamp)
                .expect("idempotent")
                .is_empty()
        );
        let mut conflict = started.clone();
        conflict.event_id = "other-event".to_owned();
        assert!(matches!(
            renderer.render(&conflict, conflict.timestamp),
            Err(TelegramGatewayError::CursorConflict(1))
        ));

        let mut altered = started.clone();
        altered.event = FrontendEvent::SubmissionAccepted;
        assert!(matches!(
            renderer.render(&altered, altered.timestamp),
            Err(TelegramGatewayError::CursorConflict(1))
        ));
    }

    #[test]
    fn renderer_streams_plain_text_and_finalizes_markdown() {
        let mut renderer =
            TelegramRenderer::new(TelegramDisplayConfig::default(), 9).expect("renderer");
        renderer
            .render(
                &event(1, FrontendEvent::Started),
                datetime!(2026-07-30 16:00 UTC),
            )
            .expect("start");
        let actions = renderer
            .render(
                &event(
                    2,
                    FrontendEvent::AssistantTextDelta {
                        text: "Fixed *two* tests.".to_owned(),
                    },
                ),
                datetime!(2026-07-30 16:00:01 UTC),
            )
            .expect("delta");
        assert!(actions.iter().any(|action| matches!(
            action,
            TelegramAction::UpsertText {
                parse_mode: TelegramParseMode::Plain,
                text,
                ..
            } if text.ends_with(" ▉")
        )));
        let final_actions = renderer
            .render(
                &event(3, FrontendEvent::TurnFinished),
                datetime!(2026-07-30 16:00:02 UTC),
            )
            .expect("finalize");
        assert!(final_actions.iter().any(|action| matches!(
            action,
            TelegramAction::UpsertText {
                parse_mode: TelegramParseMode::MarkdownV2,
                text,
                ..
            } if text.contains("\\*two\\*")
        )));
    }

    #[test]
    fn activity_and_approval_use_typed_fields() {
        let mut renderer =
            TelegramRenderer::new(TelegramDisplayConfig::default(), 9).expect("renderer");
        let activity_actions = renderer
            .render(
                &event(
                    1,
                    FrontendEvent::Activity(PresentationActivity {
                        activity_id: "activity-1".to_owned(),
                        kind: PresentationActivityKind::RepositoryRead,
                        lifecycle: PresentationLifecycle::Active,
                        title: "Reading runtime controller".to_owned(),
                        details: Vec::new(),
                        affected_paths: Vec::new(),
                        evidence_ref: None,
                    }),
                ),
                datetime!(2026-07-30 16:00 UTC),
            )
            .expect("activity");
        assert!(activity_actions.iter().any(|action| matches!(
            action,
            TelegramAction::UpsertText { text, .. } if text.starts_with("🔎")
        )));

        let approval_actions = renderer
            .render(
                &event(
                    2,
                    FrontendEvent::ApprovalRequired(PresentationApproval {
                        approval_id: "approval-1".to_owned(),
                        action: "Install package".to_owned(),
                        scope: "repository".to_owned(),
                        reason: "required for tests".to_owned(),
                        risk: "network and dependency mutation".to_owned(),
                        expires_at: datetime!(2026-07-30 16:05 UTC),
                    }),
                ),
                datetime!(2026-07-30 16:00 UTC),
            )
            .expect("approval");
        assert!(approval_actions.iter().any(|action| matches!(
            action,
            TelegramAction::UpsertText { buttons, .. } if buttons.len() == 3
        )));
    }
}
