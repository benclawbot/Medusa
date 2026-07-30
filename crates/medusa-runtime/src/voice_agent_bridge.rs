//! Bridges realtime conversation to Medusa's existing authoritative agent runtime.
//!
//! This module deliberately owns no tools, permissions, containment, audit, or
//! process control. It translates finalized voice turns and voice controls into
//! the same runtime-facing commands used by text frontends, and translates rich
//! runtime activity into concise provider-neutral voice events.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::voice::{
    RealtimeVoiceEvent, RealtimeVoiceState, TranscriptSpeaker, TranscriptUpdate, VoiceError,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentSubmission {
    pub turn_id: String,
    pub text: String,
}

/// The existing runtime submission path. Implementations must call the same
/// agent/session entry point used by typed messages.
pub trait AgentSubmissionSink {
    fn submit(&mut self, submission: AgentSubmission) -> Result<(), String>;
    fn cancel_active_task(&mut self) -> Result<bool, String>;
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VoiceAgentControl {
    StopPlayback,
    CancelResponse,
    CancelTask,
    Approve {
        request_id: String,
        response: String,
    },
    Reject {
        request_id: String,
        response: String,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AgentControlAction {
    StopPlayback,
    CancelResponse,
    CancelTask,
    Approve { request_id: String },
    Reject { request_id: String },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RuntimeVoiceActivity {
    ToolStarted { name: String },
    ToolFinished { name: String, success: bool },
    CommandStarted { summary: String },
    FileChanged { path: String },
    TestStarted { summary: String },
    Retry { summary: String },
    Progress { summary: String },
    ApprovalRequested { request_id: String, summary: String },
    Failed { summary: String, retryable: bool },
    Completed { summary: String },
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PendingApproval {
    summary: String,
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum VoiceAgentBridgeError {
    #[error("only finalized user transcripts can be submitted to the coding agent")]
    TranscriptNotFinalUser,
    #[error("voice turn text is empty")]
    EmptyTurn,
    #[error("agent submission failed: {0}")]
    Submission(String),
    #[error("no pending approval matches request `{0}`")]
    UnknownApproval(String),
    #[error("approval response was ambiguous; say `approve {0}` or `reject {0}`")]
    AmbiguousApproval(String),
    #[error("task cancellation failed: {0}")]
    Cancellation(String),
}

#[derive(Default)]
pub struct VoiceAgentBridge {
    pending_approvals: BTreeMap<String, PendingApproval>,
}

impl VoiceAgentBridge {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Submit a finalized voice turn through the same sink used by typed input.
    pub fn submit_final_transcript(
        &mut self,
        update: &TranscriptUpdate,
        sink: &mut dyn AgentSubmissionSink,
    ) -> Result<Vec<RealtimeVoiceEvent>, VoiceAgentBridgeError> {
        if update.speaker != TranscriptSpeaker::User || !update.is_final {
            return Err(VoiceAgentBridgeError::TranscriptNotFinalUser);
        }
        let text = update.text.trim();
        if text.is_empty() {
            return Err(VoiceAgentBridgeError::EmptyTurn);
        }
        sink.submit(AgentSubmission {
            turn_id: update.turn_id.clone(),
            text: text.to_owned(),
        })
        .map_err(VoiceAgentBridgeError::Submission)?;

        Ok(vec![
            RealtimeVoiceEvent::Transcript(update.clone()),
            RealtimeVoiceEvent::StateChanged {
                from: RealtimeVoiceState::Listening,
                to: RealtimeVoiceState::Thinking,
            },
        ])
    }

    pub fn runtime_activity(&mut self, activity: RuntimeVoiceActivity) -> Vec<RealtimeVoiceEvent> {
        match activity {
            RuntimeVoiceActivity::ToolStarted { name } => vec![
                RealtimeVoiceEvent::StateChanged {
                    from: RealtimeVoiceState::Thinking,
                    to: RealtimeVoiceState::ToolRunning,
                },
                RealtimeVoiceEvent::ToolActivity {
                    tool: name,
                    active: true,
                },
            ],
            RuntimeVoiceActivity::ToolFinished { name, success } => vec![
                RealtimeVoiceEvent::ToolActivity {
                    tool: name,
                    active: false,
                },
                RealtimeVoiceEvent::TransportStatus {
                    connected: true,
                    detail: Some(if success {
                        "Tool work completed".to_owned()
                    } else {
                        "Tool work failed; reviewing recovery options".to_owned()
                    }),
                },
            ],
            RuntimeVoiceActivity::CommandStarted { summary }
            | RuntimeVoiceActivity::TestStarted { summary }
            | RuntimeVoiceActivity::Retry { summary }
            | RuntimeVoiceActivity::Progress { summary } => {
                vec![RealtimeVoiceEvent::TransportStatus {
                    connected: true,
                    detail: Some(concise_summary(&summary)),
                }]
            }
            RuntimeVoiceActivity::FileChanged { path } => {
                vec![RealtimeVoiceEvent::TransportStatus {
                    connected: true,
                    detail: Some(format!("Updated {}", concise_path(&path))),
                }]
            }
            RuntimeVoiceActivity::ApprovalRequested {
                request_id,
                summary,
            } => {
                self.pending_approvals.insert(
                    request_id.clone(),
                    PendingApproval {
                        summary: summary.clone(),
                    },
                );
                vec![
                    RealtimeVoiceEvent::StateChanged {
                        from: RealtimeVoiceState::ToolRunning,
                        to: RealtimeVoiceState::AwaitingApproval,
                    },
                    RealtimeVoiceEvent::ApprovalRequired {
                        request_id,
                        summary,
                    },
                ]
            }
            RuntimeVoiceActivity::Failed { summary, retryable } => vec![
                RealtimeVoiceEvent::StateChanged {
                    from: RealtimeVoiceState::ToolRunning,
                    to: RealtimeVoiceState::Failed,
                },
                RealtimeVoiceEvent::Error(VoiceError {
                    code: "agent_runtime_failed".to_owned(),
                    message: concise_summary(&summary),
                    retryable,
                }),
            ],
            RuntimeVoiceActivity::Completed { summary } => vec![
                RealtimeVoiceEvent::TransportStatus {
                    connected: true,
                    detail: Some(concise_summary(&summary)),
                },
                RealtimeVoiceEvent::StateChanged {
                    from: RealtimeVoiceState::ToolRunning,
                    to: RealtimeVoiceState::Listening,
                },
            ],
        }
    }

    pub fn control(
        &mut self,
        control: VoiceAgentControl,
        sink: &mut dyn AgentSubmissionSink,
    ) -> Result<AgentControlAction, VoiceAgentBridgeError> {
        match control {
            VoiceAgentControl::StopPlayback => Ok(AgentControlAction::StopPlayback),
            VoiceAgentControl::CancelResponse => Ok(AgentControlAction::CancelResponse),
            VoiceAgentControl::CancelTask => {
                sink.cancel_active_task()
                    .map_err(VoiceAgentBridgeError::Cancellation)?;
                Ok(AgentControlAction::CancelTask)
            }
            VoiceAgentControl::Approve {
                request_id,
                response,
            } => self.resolve_approval(request_id, response, true),
            VoiceAgentControl::Reject {
                request_id,
                response,
            } => self.resolve_approval(request_id, response, false),
        }
    }

    fn resolve_approval(
        &mut self,
        request_id: String,
        response: String,
        approve: bool,
    ) -> Result<AgentControlAction, VoiceAgentBridgeError> {
        let pending = self
            .pending_approvals
            .get(&request_id)
            .ok_or_else(|| VoiceAgentBridgeError::UnknownApproval(request_id.clone()))?;
        let expected = if approve {
            format!("approve {request_id}")
        } else {
            format!("reject {request_id}")
        };
        if !response.trim().eq_ignore_ascii_case(&expected) {
            return Err(VoiceAgentBridgeError::AmbiguousApproval(request_id));
        }
        let _ = &pending.summary;
        self.pending_approvals.remove(&request_id);
        Ok(if approve {
            AgentControlAction::Approve { request_id }
        } else {
            AgentControlAction::Reject { request_id }
        })
    }
}

fn concise_summary(summary: &str) -> String {
    const MAX: usize = 180;
    let normalized = summary.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.chars().count() <= MAX {
        return normalized;
    }
    let mut shortened = normalized.chars().take(MAX - 1).collect::<String>();
    shortened.push('…');
    shortened
}

fn concise_path(path: &str) -> String {
    path.rsplit(['/', '\\']).next().unwrap_or(path).to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct RecordingSink {
        submissions: Vec<AgentSubmission>,
        cancellations: usize,
    }

    impl AgentSubmissionSink for RecordingSink {
        fn submit(&mut self, submission: AgentSubmission) -> Result<(), String> {
            self.submissions.push(submission);
            Ok(())
        }

        fn cancel_active_task(&mut self) -> Result<bool, String> {
            self.cancellations += 1;
            Ok(true)
        }
    }

    #[test]
    fn finalized_voice_turn_uses_authoritative_submission_sink() {
        let mut bridge = VoiceAgentBridge::new();
        let mut sink = RecordingSink::default();
        let update = TranscriptUpdate {
            turn_id: "voice-1".to_owned(),
            speaker: TranscriptSpeaker::User,
            text: " run the existing tests ".to_owned(),
            is_final: true,
        };

        let events = bridge
            .submit_final_transcript(&update, &mut sink)
            .expect("submit voice turn");
        assert_eq!(
            sink.submissions,
            vec![AgentSubmission {
                turn_id: "voice-1".to_owned(),
                text: "run the existing tests".to_owned(),
            }]
        );
        assert!(matches!(events[0], RealtimeVoiceEvent::Transcript(_)));
    }

    #[test]
    fn partial_or_assistant_transcripts_never_invoke_agent() {
        let mut bridge = VoiceAgentBridge::new();
        let mut sink = RecordingSink::default();
        for update in [
            TranscriptUpdate {
                turn_id: "partial".to_owned(),
                speaker: TranscriptSpeaker::User,
                text: "maybe".to_owned(),
                is_final: false,
            },
            TranscriptUpdate {
                turn_id: "assistant".to_owned(),
                speaker: TranscriptSpeaker::Assistant,
                text: "status".to_owned(),
                is_final: true,
            },
        ] {
            assert_eq!(
                bridge.submit_final_transcript(&update, &mut sink),
                Err(VoiceAgentBridgeError::TranscriptNotFinalUser)
            );
        }
        assert!(sink.submissions.is_empty());
    }

    #[test]
    fn vague_acknowledgement_cannot_approve_sensitive_action() {
        let mut bridge = VoiceAgentBridge::new();
        let mut sink = RecordingSink::default();
        bridge.runtime_activity(RuntimeVoiceActivity::ApprovalRequested {
            request_id: "approval-7".to_owned(),
            summary: "Run a privileged command".to_owned(),
        });

        assert_eq!(
            bridge.control(
                VoiceAgentControl::Approve {
                    request_id: "approval-7".to_owned(),
                    response: "yes, sounds good".to_owned(),
                },
                &mut sink,
            ),
            Err(VoiceAgentBridgeError::AmbiguousApproval(
                "approval-7".to_owned()
            ))
        );
        assert_eq!(
            bridge
                .control(
                    VoiceAgentControl::Approve {
                        request_id: "approval-7".to_owned(),
                        response: "approve approval-7".to_owned(),
                    },
                    &mut sink,
                )
                .expect("explicit approval"),
            AgentControlAction::Approve {
                request_id: "approval-7".to_owned()
            }
        );
    }

    #[test]
    fn playback_response_and_task_cancellation_are_distinct() {
        let mut bridge = VoiceAgentBridge::new();
        let mut sink = RecordingSink::default();
        assert_eq!(
            bridge
                .control(VoiceAgentControl::StopPlayback, &mut sink)
                .expect("stop playback"),
            AgentControlAction::StopPlayback
        );
        assert_eq!(
            bridge
                .control(VoiceAgentControl::CancelResponse, &mut sink)
                .expect("cancel response"),
            AgentControlAction::CancelResponse
        );
        assert_eq!(sink.cancellations, 0);
        assert_eq!(
            bridge
                .control(VoiceAgentControl::CancelTask, &mut sink)
                .expect("cancel task"),
            AgentControlAction::CancelTask
        );
        assert_eq!(sink.cancellations, 1);
    }

    #[test]
    fn runtime_updates_are_concise_and_do_not_read_raw_logs() {
        let mut bridge = VoiceAgentBridge::new();
        let events = bridge.runtime_activity(RuntimeVoiceActivity::Progress {
            summary: format!("{} secret-token", "long output ".repeat(30)),
        });
        let RealtimeVoiceEvent::TransportStatus {
            detail: Some(detail),
            ..
        } = &events[0]
        else {
            panic!("expected concise progress event");
        };
        assert!(detail.chars().count() <= 180);
    }
}
