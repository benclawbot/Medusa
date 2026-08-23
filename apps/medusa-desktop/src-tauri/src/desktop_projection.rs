use medusa_protocol::frontend::{
    FrontendEvent, FrontendEventEnvelope, PresentationActivity, PresentationActivityKind,
    PresentationLifecycle,
};

struct DesktopCanonicalPresentation {
    pending: VecDeque<DesktopRuntimeEvent>,
    run_active: bool,
}

impl DesktopCanonicalPresentation {
    fn new() -> Self {
        Self {
            pending: VecDeque::new(),
            run_active: false,
        }
    }

    fn reset(&mut self) {
        self.pending.clear();
        self.run_active = false;
    }

    fn push(&mut self, envelopes: Vec<FrontendEventEnvelope>) {
        for envelope in envelopes {
            self.pending
                .extend(map_frontend_event(envelope, &mut self.run_active));
        }
    }

    fn push_transient(&mut self, event: FrontendTransientEvent) {
        self.pending.push_back(map_transient_event(event));
    }

    fn try_event(&mut self) -> Option<DesktopRuntimeEvent> {
        self.pending.pop_front()
    }
}

fn map_transient_event(event: FrontendTransientEvent) -> DesktopRuntimeEvent {
    match event {
        FrontendTransientEvent::RecoveryAvailable { recovery } => {
            DesktopRuntimeEvent::RecoveryAvailable { recovery }
        }
        FrontendTransientEvent::RecoveryCompleted { record, audit_path } => {
            DesktopRuntimeEvent::RecoveryCompleted { record, audit_path }
        }
        FrontendTransientEvent::Settings {
            model,
            effort,
            plan_mode,
            credential_configured,
        } => DesktopRuntimeEvent::Settings {
            model,
            effort,
            plan_mode,
            credential_configured,
        },
        FrontendTransientEvent::ConfigurationChanged {
            revision,
            active_profile,
            changed_keys,
            origin,
            apply_timing,
        } => DesktopRuntimeEvent::ConfigurationChanged {
            revision,
            active_profile,
            changed_keys,
            origin,
            apply_timing,
        },
        FrontendTransientEvent::Notice { title, details } => {
            DesktopRuntimeEvent::Notice { title, details }
        }
        FrontendTransientEvent::NewSession => DesktopRuntimeEvent::NewSession,
        FrontendTransientEvent::Progress { turn } => DesktopRuntimeEvent::Progress { turn },
        FrontendTransientEvent::Failed { message } => DesktopRuntimeEvent::Failed { message },
    }
}

fn map_frontend_event(
    envelope: FrontendEventEnvelope,
    run_active: &mut bool,
) -> VecDeque<DesktopRuntimeEvent> {
    let FrontendEventEnvelope {
        session_id, event, ..
    } = envelope;
    let mut events = VecDeque::new();
    match event {
        FrontendEvent::SubmissionAccepted | FrontendEvent::Started => {
            if let Some(event) = canonical_start_event(run_active) {
                events.push_back(event);
            }
        }
        FrontendEvent::SubmissionQueued { position } => {
            events.push_back(DesktopRuntimeEvent::Notice {
                title: "Follow-up queued".to_owned(),
                details: vec![format!("Queue position: {position}")],
            });
        }
        FrontendEvent::AssistantTextDelta { text }
        | FrontendEvent::AssistantInterim { text } => {
            events.push_back(DesktopRuntimeEvent::AssistantText { text });
        }
        FrontendEvent::Activity(activity) => {
            events.push_back(DesktopRuntimeEvent::Activity {
                activity: map_activity(activity),
            });
        }
        FrontendEvent::Team {
            workers,
            verification,
        } => {
            for worker in workers {
                let mut details = vec![format!("role {}", worker.role)];
                if let Some(action) = worker.current_action {
                    details.push(action);
                }
                events.push_back(DesktopRuntimeEvent::Activity {
                    activity: crate::dto::DesktopActivity {
                        id: Some(format!("team:{}", worker.worker_id)),
                        kind: activity_kind(PresentationActivityKind::Worker, worker.lifecycle),
                        title: format!("{} · {}", worker.worker_id, worker.task),
                        details,
                    },
                });
            }
            if let Some(verification) = verification {
                events.push_back(DesktopRuntimeEvent::Activity {
                    activity: crate::dto::DesktopActivity {
                        id: Some("team-verification".to_owned()),
                        kind: crate::dto::DesktopActivityKind::Verification,
                        title: "Team verification".to_owned(),
                        details: vec![verification],
                    },
                });
            }
        }
        FrontendEvent::Plan { steps, .. } => {
            events.push_back(DesktopRuntimeEvent::Plan {
                steps: steps
                    .into_iter()
                    .map(|step| crate::dto::DesktopPlanStep {
                        title: step.title,
                        status: match step.lifecycle {
                            PresentationLifecycle::Active => {
                                crate::dto::DesktopPlanStepStatus::InProgress
                            }
                            PresentationLifecycle::Succeeded => {
                                crate::dto::DesktopPlanStepStatus::Completed
                            }
                            PresentationLifecycle::Failed | PresentationLifecycle::Cancelled => {
                                crate::dto::DesktopPlanStepStatus::Failed
                            }
                            PresentationLifecycle::Waiting
                            | PresentationLifecycle::Informational => {
                                crate::dto::DesktopPlanStepStatus::Pending
                            }
                        },
                    })
                    .collect(),
            });
        }
        FrontendEvent::Question(question) => {
            events.push_back(DesktopRuntimeEvent::Question {
                prompts: vec![crate::dto::DesktopQuestionPrompt {
                    header: "Question".to_owned(),
                    question: question.prompt,
                    options: question
                        .options
                        .into_iter()
                        .map(|option| crate::dto::DesktopQuestionOption {
                            description: if option.value != option.label {
                                option.value
                            } else {
                                String::new()
                            },
                            label: option.label,
                        })
                        .collect(),
                    multi_select: false,
                }],
            });
        }
        FrontendEvent::ApprovalRequired(approval) => {
            events.push_back(DesktopRuntimeEvent::Question {
                prompts: vec![crate::dto::DesktopQuestionPrompt {
                    header: "Approval".to_owned(),
                    question: format!(
                        "{} in {}: {} (risk: {})",
                        approval.action, approval.scope, approval.reason, approval.risk
                    ),
                    options: vec![
                        crate::dto::DesktopQuestionOption {
                            label: "Approve".to_owned(),
                            description: "Allow this action once".to_owned(),
                        },
                        crate::dto::DesktopQuestionOption {
                            label: "Deny".to_owned(),
                            description: "Do not perform this action".to_owned(),
                        },
                    ],
                    multi_select: false,
                }],
            });
        }
        FrontendEvent::Usage {
            input_tokens,
            output_tokens,
            cache_read_input_tokens,
            cache_creation_input_tokens,
            total_tokens,
            duration_ms,
            tokens_per_second_milli,
            estimated_cost_microusd,
            provenance,
        } => {
            events.push_back(DesktopRuntimeEvent::Usage {
                input_tokens,
                output_tokens,
                cache_read_input_tokens,
                cache_creation_input_tokens,
                total_tokens,
                duration_ms,
                tokens_per_second_milli,
                estimated_cost_microusd,
                provenance,
            });
        }
        FrontendEvent::Progress { turn, .. } => {
            events.push_back(DesktopRuntimeEvent::Progress { turn });
        }
        FrontendEvent::SettingsChanged { .. } => {}
        FrontendEvent::Notice {
            severity,
            title,
            mut details,
        } => {
            if severity != "info" {
                details.insert(0, format!("Severity: {severity}"));
            }
            if title == "Conversation compacted" {
                events.push_back(DesktopRuntimeEvent::Compacted {
                    message: details.join(" · "),
                });
            } else {
                events.push_back(DesktopRuntimeEvent::Notice { title, details });
            }
        }
        FrontendEvent::Artifact(artifact) => {
            let mut details = vec![
                format!("Media type: {}", artifact.media_type),
                format!("Evidence: {}", artifact.evidence_ref),
            ];
            if let Some(caption) = artifact.caption {
                details.push(caption);
            }
            events.push_back(DesktopRuntimeEvent::Activity {
                activity: crate::dto::DesktopActivity {
                    id: Some(artifact.artifact_id),
                    kind: crate::dto::DesktopActivityKind::Done,
                    title: format!("Artifact available: {}", artifact.name),
                    details,
                },
            });
        }
        FrontendEvent::TurnFinished => {
            *run_active = false;
            events.push_back(DesktopRuntimeEvent::TurnFinished);
        }
        FrontendEvent::Completed { summary } => {
            *run_active = false;
            if let Some(summary) = summary {
                events.push_back(DesktopRuntimeEvent::Notice {
                    title: "Completion report".to_owned(),
                    details: vec![summary],
                });
            }
            events.push_back(DesktopRuntimeEvent::Completed { session_id });
        }
        FrontendEvent::Cancelled { reason } => {
            *run_active = false;
            if let Some(reason) = reason {
                events.push_back(DesktopRuntimeEvent::Notice {
                    title: "Cancellation reason".to_owned(),
                    details: vec![reason],
                });
            }
            events.push_back(DesktopRuntimeEvent::Cancelled);
        }
        FrontendEvent::Failed { message, recovery } => {
            *run_active = false;
            let message = if recovery.is_empty() {
                message
            } else {
                format!("{message}\nRecovery: {}", recovery.join("; "))
            };
            events.push_back(DesktopRuntimeEvent::Failed { message });
        }
    }
    events
}

fn canonical_start_event(run_active: &mut bool) -> Option<DesktopRuntimeEvent> {
    if *run_active {
        None
    } else {
        *run_active = true;
        Some(DesktopRuntimeEvent::Started)
    }
}

fn map_activity(activity: PresentationActivity) -> crate::dto::DesktopActivity {
    let mut details = activity.details;
    if !activity.affected_paths.is_empty() {
        details.push(format!("Paths: {}", activity.affected_paths.join(", ")));
    }
    if let Some(evidence) = activity.evidence_ref {
        details.push(format!("Evidence: {evidence}"));
    }
    crate::dto::DesktopActivity {
        id: Some(activity.activity_id),
        kind: activity_kind(activity.kind, activity.lifecycle),
        title: activity.title,
        details,
    }
}

fn activity_kind(
    kind: PresentationActivityKind,
    lifecycle: PresentationLifecycle,
) -> crate::dto::DesktopActivityKind {
    match lifecycle {
        PresentationLifecycle::Failed | PresentationLifecycle::Cancelled => {
            crate::dto::DesktopActivityKind::Error
        }
        PresentationLifecycle::Succeeded => match kind {
            PresentationActivityKind::Assistant => crate::dto::DesktopActivityKind::Assistant,
            PresentationActivityKind::Verification | PresentationActivityKind::Test => {
                crate::dto::DesktopActivityKind::Verification
            }
            PresentationActivityKind::Error => crate::dto::DesktopActivityKind::Error,
            _ => crate::dto::DesktopActivityKind::Done,
        },
        PresentationLifecycle::Active
        | PresentationLifecycle::Waiting
        | PresentationLifecycle::Informational => match kind {
            PresentationActivityKind::Assistant => crate::dto::DesktopActivityKind::Assistant,
            PresentationActivityKind::RepositoryRead
            | PresentationActivityKind::Edit
            | PresentationActivityKind::Command => crate::dto::DesktopActivityKind::Tool,
            PresentationActivityKind::Verification | PresentationActivityKind::Test => {
                crate::dto::DesktopActivityKind::Verification
            }
            PresentationActivityKind::Done => crate::dto::DesktopActivityKind::Done,
            PresentationActivityKind::Error => crate::dto::DesktopActivityKind::Error,
            _ => crate::dto::DesktopActivityKind::Progress,
        },
    }
}

#[cfg(test)]
mod desktop_projection_tests {
    use medusa_protocol::frontend::{
        FRONTEND_PROTOCOL_VERSION, FrontendEvent, FrontendEventEnvelope, PresentationActivity,
        PresentationActivityKind, PresentationLifecycle, PresentationPlanStep,
    };
    use time::OffsetDateTime;

    use super::*;

    fn envelope(event: FrontendEvent) -> FrontendEventEnvelope {
        FrontendEventEnvelope {
            protocol_version: FRONTEND_PROTOCOL_VERSION,
            event_id: "event-1:desktop".to_owned(),
            cursor: 1,
            session_id: "session-1".to_owned(),
            turn_id: Some("turn-1".to_owned()),
            parent_event_id: None,
            correlation_id: "correlation-1".to_owned(),
            timestamp: OffsetDateTime::UNIX_EPOCH,
            lifecycle: PresentationLifecycle::Active,
            event,
        }
    }

    #[test]
    fn canonical_start_is_emitted_once_until_terminal_state() {
        let mut run_active = false;
        assert!(matches!(
            canonical_start_event(&mut run_active),
            Some(DesktopRuntimeEvent::Started)
        ));
        assert!(canonical_start_event(&mut run_active).is_none());
        run_active = false;
        assert!(matches!(
            canonical_start_event(&mut run_active),
            Some(DesktopRuntimeEvent::Started)
        ));
    }

    #[test]
    fn transient_settings_keep_the_desktop_contract() {
        assert!(matches!(
            map_transient_event(FrontendTransientEvent::Settings {
                model: "MiniMax-M3".to_owned(),
                effort: "effort:high".to_owned(),
                plan_mode: false,
                credential_configured: true,
            }),
            DesktopRuntimeEvent::Settings {
                credential_configured: true,
                ..
            }
        ));
    }

    #[test]
    fn canonical_plan_and_activity_keep_the_desktop_contract() {
        let mut run_active = false;
        let plan = map_frontend_event(
            envelope(FrontendEvent::Plan {
                steps: vec![PresentationPlanStep {
                    step_id: "step-1".to_owned(),
                    title: "Wire desktop".to_owned(),
                    lifecycle: PresentationLifecycle::Active,
                }],
                current: Some("step-1".to_owned()),
            }),
            &mut run_active,
        );
        assert!(matches!(
            plan.front(),
            Some(DesktopRuntimeEvent::Plan { steps })
                if matches!(steps[0].status, crate::dto::DesktopPlanStepStatus::InProgress)
        ));

        let activity = map_frontend_event(
            envelope(FrontendEvent::Activity(PresentationActivity {
                activity_id: "verify-1".to_owned(),
                kind: PresentationActivityKind::Verification,
                lifecycle: PresentationLifecycle::Succeeded,
                title: "Desktop package".to_owned(),
                details: vec!["passed".to_owned()],
                affected_paths: vec!["apps/medusa-desktop".to_owned()],
                evidence_ref: Some("evidence-1".to_owned()),
            })),
            &mut run_active,
        );
        assert!(matches!(
            activity.front(),
            Some(DesktopRuntimeEvent::Activity { activity })
                if matches!(activity.kind, crate::dto::DesktopActivityKind::Verification)
                    && activity.id.as_deref() == Some("verify-1")
        ));
    }

    #[test]
    fn canonical_usage_keeps_normalized_telemetry() {
        let mut run_active = false;
        let usage = map_frontend_event(
            envelope(FrontendEvent::Usage {
                input_tokens: 11,
                output_tokens: 7,
                cache_read_input_tokens: 3,
                cache_creation_input_tokens: 2,
                total_tokens: 23,
                duration_ms: 900,
                tokens_per_second_milli: 25_555,
                estimated_cost_microusd: 123,
                provenance: "provider_reported".to_owned(),
            }),
            &mut run_active,
        );
        assert!(matches!(
            usage.front(),
            Some(DesktopRuntimeEvent::Usage {
                input_tokens: 11,
                output_tokens: 7,
                cache_read_input_tokens: 3,
                cache_creation_input_tokens: 2,
                total_tokens: 23,
                duration_ms: 900,
                tokens_per_second_milli: 25_555,
                estimated_cost_microusd: 123,
                provenance,
            }) if provenance == "provider_reported"
        ));
    }
}
