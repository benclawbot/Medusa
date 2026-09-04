use super::*;

#[cfg(test)]
pub(super) fn execute_slash_command(
    state: &mut RuntimeState,
    command: SlashCommand,
    events: &Sender<RuntimeEvent>,
    cancel: &Arc<AtomicBool>,
) -> Result<Option<RuntimeEvent>, RuntimeError> {
    let submission = Arc::new(Mutex::new(SubmissionState {
        busy: true,
        ..SubmissionState::default()
    }));
    execute_slash_command_with_submission(state, command, events, cancel, &submission)
}

pub(super) fn execute_slash_command_with_submission(
    state: &mut RuntimeState,
    command: SlashCommand,
    events: &Sender<RuntimeEvent>,
    cancel: &Arc<AtomicBool>,
    submission: &Arc<Mutex<SubmissionState>>,
) -> Result<Option<RuntimeEvent>, RuntimeError> {
    match command {
        SlashCommand::Config(command) => {
            crate::config_command::execute(state, command, events)?;
        }
        SlashCommand::Team(_) => {
            return Err(RuntimeError::InvalidCommand(
                "team commands must execute through the live control plane".to_owned(),
            ));
        }
        SlashCommand::Learning { action } => {
            let mut authority =
                crate::learning_authority::open(&state.repo).map_err(RuntimeError::agent)?;
            let snapshot = authority
                .snapshot()
                .map_err(|error| RuntimeError::agent(error.to_string()))?;
            match action {
                LearningCommand::Show { filter } => {
                    let details = crate::learning_authority::details(&snapshot, filter.as_deref());
                    let _ = events.send(RuntimeEvent::Notice {
                        title: "Canonical learning authority".to_owned(),
                        details,
                    });
                }
                LearningCommand::Inspect { id } => {
                    let details = crate::learning_authority::inspect(&authority, &id)
                        .map_err(RuntimeError::agent)?;
                    let _ = events.send(RuntimeEvent::Notice {
                        title: "Canonical refinement inspection".to_owned(),
                        details,
                    });
                }
                LearningCommand::Propose { scope, key, value } => {
                    let updated =
                        crate::learning_authority::propose(&mut authority, &scope, &key, &value)
                            .map_err(RuntimeError::agent)?;
                    let _ = events.send(RuntimeEvent::Notice {
                        title: "Canonical refinement proposed".to_owned(),
                        details: vec![format!(
                            "{} v{} is proposed at authority revision {}",
                            updated
                                .records
                                .last()
                                .map_or("unknown", |record| record.proposal_id.as_str()),
                            updated.revision,
                            updated.revision
                        )],
                    });
                }
                LearningCommand::Evaluate {
                    id,
                    validation_passed,
                    regression_passed,
                    effectiveness_passed,
                } => {
                    let updated = crate::learning_authority::evaluate(
                        &mut authority,
                        &id,
                        validation_passed,
                        regression_passed,
                        effectiveness_passed,
                    )
                    .map_err(RuntimeError::agent)?;
                    let _ = events.send(RuntimeEvent::Notice {
                        title: "Canonical refinement evaluated".to_owned(),
                        details: vec![format!(
                            "validation={} regression={} effectiveness={} at authority revision {}",
                            if validation_passed { "pass" } else { "fail" },
                            if regression_passed { "pass" } else { "fail" },
                            if effectiveness_passed { "pass" } else { "fail" },
                            updated.revision
                        )],
                    });
                }
                LearningCommand::Privacy => {
                    let legacy_snapshot = crate::learning_review::read(&state.repo)
                        .map_err(|error| RuntimeError::agent(error.to_string()))?;
                    let preview = crate::learning_review::redaction_preview(&state.repo)
                        .map_err(|error| RuntimeError::agent(error.to_string()))?;
                    let _ = events.send(RuntimeEvent::Notice {
                        title: "Learning privacy".to_owned(),
                        details: vec![
                            format!("Capture: {}", legacy_snapshot.privacy.capture_enabled),
                            format!(
                                "User-level persistence: {}",
                                legacy_snapshot.privacy.user_persistence_enabled
                            ),
                            format!(
                                "Cross-repository reuse: {}",
                                legacy_snapshot.privacy.cross_repository_reuse_enabled
                            ),
                            format!("Telemetry: {}", legacy_snapshot.privacy.telemetry_enabled),
                            format!(
                                "Automatic proposals: {}",
                                legacy_snapshot.privacy.automatic_proposals_enabled
                            ),
                            format!("Export redaction safe: {}", preview.safe),
                            preview.warnings.join(" "),
                        ],
                    });
                }
                LearningCommand::Export => {
                    let json = String::from_utf8(
                        authority
                            .export()
                            .map_err(|error| RuntimeError::agent(error.to_string()))?,
                    )
                    .map_err(RuntimeError::agent)?;
                    let _ = events.send(RuntimeEvent::Notice {
                        title: "Canonical refinement authority export".to_owned(),
                        details: json.lines().map(str::to_owned).collect(),
                    });
                }
                LearningCommand::Approve { id } => {
                    let updated = crate::learning_authority::transition(
                        &mut authority,
                        &id,
                        crate::learning_authority::RuntimeLearningAction::Approve,
                    )
                    .map_err(RuntimeError::agent)?;
                    let _ = events.send(RuntimeEvent::Notice {
                        title: "Canonical refinement lifecycle updated".to_owned(),
                        details: vec![format!(
                            "{} was approved at authority revision {}",
                            id, updated.revision
                        )],
                    });
                }
                action @ (LearningCommand::Reject { .. }
                | LearningCommand::Defer { .. }
                | LearningCommand::Validate { .. }
                | LearningCommand::Activate { .. }
                | LearningCommand::Suspend { .. }
                | LearningCommand::Rollback { .. }
                | LearningCommand::Delete { .. }) => {
                    let (id, action) = match action {
                        LearningCommand::Reject { id } => {
                            (id, crate::learning_authority::RuntimeLearningAction::Reject)
                        }
                        LearningCommand::Defer { id } => {
                            (id, crate::learning_authority::RuntimeLearningAction::Defer)
                        }
                        LearningCommand::Validate { id } => (
                            id,
                            crate::learning_authority::RuntimeLearningAction::Validate,
                        ),
                        LearningCommand::Activate { id } => (
                            id,
                            crate::learning_authority::RuntimeLearningAction::Activate,
                        ),
                        LearningCommand::Suspend { id } => (
                            id,
                            crate::learning_authority::RuntimeLearningAction::Suspend,
                        ),
                        LearningCommand::Rollback { id } => (
                            id,
                            crate::learning_authority::RuntimeLearningAction::Rollback,
                        ),
                        LearningCommand::Delete { id } => {
                            (id, crate::learning_authority::RuntimeLearningAction::Delete)
                        }
                        _ => unreachable!(),
                    };
                    let updated =
                        crate::learning_authority::transition(&mut authority, &id, action)
                            .map_err(RuntimeError::agent)?;
                    let _ = events.send(RuntimeEvent::Notice {
                        title: "Canonical refinement lifecycle updated".to_owned(),
                        details: vec![format!(
                            "{} transitioned at authority revision {}",
                            id, updated.revision
                        )],
                    });
                }
            }
        }
        SlashCommand::Review { action } => {
            let workspace = crate::review::read_review_workspace(&state.repo)
                .map_err(|error| RuntimeError::agent(error.to_string()))?;
            match action {
                ReviewCommand::Show { filter } => {
                    let mut details = Vec::new();
                    for file in &workspace.snapshot.files {
                        let label =
                            format!("{:?} {:?} {:?}", file.kind, file.origin, file.verification)
                                .to_ascii_lowercase();
                        if filter.as_ref().is_some_and(|value| {
                            !file.path.contains(value)
                                && !label.contains(&value.to_ascii_lowercase())
                        }) {
                            continue;
                        }
                        details.push(format!(
                            "{} | {:?} | {:?} | {:?}",
                            file.path, file.kind, file.origin, file.review_state
                        ));
                        if let Some(diff) = workspace
                            .files
                            .iter()
                            .find(|candidate| candidate.path == file.path)
                        {
                            for hunk in &diff.hunks {
                                details.push(format!("  hunk {} {}", hunk.id, hunk.header));
                            }
                        }
                    }
                    if details.is_empty() {
                        details.push("No changes match the review filter.".to_owned());
                    }
                    let _ = events.send(RuntimeEvent::Notice {
                        title: "Repository review".to_owned(),
                        details,
                    });
                }
                ReviewCommand::AcceptFile { path } => {
                    crate::review::apply_review_action(
                        &state.repo,
                        medusa_review_model::ReviewActionRequest::AcceptFile {
                            path,
                            expected_snapshot_id: workspace.snapshot.id.clone(),
                        },
                        "tui",
                    )
                    .map_err(|error| RuntimeError::agent(error.to_string()))?;
                    let _ = events.send(RuntimeEvent::Notice {
                        title: "Review accepted".to_owned(),
                        details: vec![
                            "File marked accepted; no commit, push, or merge was performed."
                                .to_owned(),
                        ],
                    });
                }
                ReviewCommand::AcceptTask => {
                    crate::review::apply_review_action(
                        &state.repo,
                        medusa_review_model::ReviewActionRequest::AcceptTask {
                            expected_snapshot_id: workspace.snapshot.id.clone(),
                        },
                        "tui",
                    )
                    .map_err(|error| RuntimeError::agent(error.to_string()))?;
                    let _ = events.send(RuntimeEvent::Notice { title: "Task review accepted".to_owned(), details: vec!["All eligible Medusa changes were marked accepted; repository history was not modified.".to_owned()] });
                }
                ReviewCommand::RevertFile { path } => {
                    let file = workspace
                        .snapshot
                        .file(&path)
                        .ok_or_else(|| RuntimeError::agent("review file not found"))?;
                    crate::review::apply_review_action(
                        &state.repo,
                        medusa_review_model::ReviewActionRequest::RevertFile {
                            path,
                            expected_snapshot_id: workspace.snapshot.id.clone(),
                            expected_file_fingerprint: file.current_fingerprint.clone(),
                        },
                        "tui",
                    )
                    .map_err(|error| RuntimeError::agent(error.to_string()))?;
                    let _ = events.send(RuntimeEvent::Notice {
                        title: "File reverted".to_owned(),
                        details: vec![
                            "The selected Medusa file change was reverted safely.".to_owned(),
                        ],
                    });
                }
                ReviewCommand::RevertHunk { path, hunk_id } => {
                    let file = workspace
                        .snapshot
                        .file(&path)
                        .ok_or_else(|| RuntimeError::agent("review file not found"))?;
                    let hunk = file
                        .hunks
                        .iter()
                        .find(|candidate| candidate.id == hunk_id)
                        .ok_or_else(|| RuntimeError::agent("review hunk not found"))?;
                    crate::review::apply_review_action(
                        &state.repo,
                        medusa_review_model::ReviewActionRequest::RevertHunk {
                            path,
                            hunk_id,
                            expected_snapshot_id: workspace.snapshot.id.clone(),
                            expected_file_fingerprint: file.current_fingerprint.clone(),
                            expected_hunk_fingerprint: hunk.current_fingerprint.clone(),
                        },
                        "tui",
                    )
                    .map_err(|error| RuntimeError::agent(error.to_string()))?;
                    let _ = events.send(RuntimeEvent::Notice {
                        title: "Hunk reverted".to_owned(),
                        details: vec!["The selected Medusa hunk was reverted safely.".to_owned()],
                    });
                }
                ReviewCommand::Export => {
                    let generated_at = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_millis() as i64;
                    let export = crate::review::export_review_audit(&state.repo, generated_at)
                        .map_err(|error| RuntimeError::agent(error.to_string()))?;
                    let json = serde_json::to_string_pretty(&export)
                        .map_err(|error| RuntimeError::agent(error.to_string()))?;
                    let _ = events.send(RuntimeEvent::Notice {
                        title: "Review audit export".to_owned(),
                        details: json.lines().map(str::to_owned).collect(),
                    });
                }
            }
        }
        SlashCommand::Help => {
            let _ = events.send(RuntimeEvent::Notice {
                title: "Slash commands".to_owned(),
                details: commands::COMMAND_SPECS
                    .iter()
                    .map(|spec| format!("{} - {}", spec.usage, spec.description))
                    .collect(),
            });
        }
        SlashCommand::New => {
            if let Some(session) = state.session.as_mut() {
                medusa_agent::record_session_event(
                    session,
                    Actor::Coordinator,
                    EventPayload::SessionReset {
                        reason: "user requested a new session".to_owned(),
                    },
                )
                .map_err(RuntimeError::agent)?;
            }
            state.session = None;
            lock_submission(submission).active_session_id = None;
            state.pending_goal = None;
            state.pending_skill = None;
            state.config.agent.mode = state.base_config.agent.mode;
            state.plan_mode = false;
            let _ = events.send(RuntimeEvent::NewSession);
            let _ = events.send(state.settings_event());
        }
        SlashCommand::Compact { focus } => {
            let Some(session) = state.session.as_mut() else {
                let _ = events.send(RuntimeEvent::Notice {
                    title: "Nothing to compact".to_owned(),
                    details: vec!["Start a task before compacting its context.".to_owned()],
                });
                return Ok(None);
            };
            let original_messages = session.messages.len();
            compact_session(session, focus.as_deref()).map_err(RuntimeError::agent)?;
            let _ = events.send(RuntimeEvent::Compacted {
                message: format!(
                    "Compacted session context from {original_messages} messages to a durable summary."
                ),
            });
        }
        SlashCommand::Goal { objective } => match objective {
            Some(objective) => {
                if let Some(session) = state.session.as_mut() {
                    update_session_objective(session, objective.clone())
                        .map_err(RuntimeError::agent)?;
                } else {
                    state.pending_goal = Some(objective.clone());
                }
                let _ = events.send(RuntimeEvent::Notice {
                    title: "Goal updated".to_owned(),
                    details: vec![
                        objective,
                        "The goal will be included in the next agent turn.".to_owned(),
                    ],
                });
            }
            None => {
                let objective = state
                    .session
                    .as_ref()
                    .map(|session| session.objective.as_str())
                    .or(state.pending_goal.as_deref())
                    .unwrap_or("No goal is set; the next prompt becomes the session goal.");
                let _ = events.send(RuntimeEvent::Notice {
                    title: "Current goal".to_owned(),
                    details: vec![objective.to_owned()],
                });
            }
        },
        SlashCommand::Model(model_command) => match model_command {
            ModelCommand::Show => {
                let _ = events.send(RuntimeEvent::Notice {
                    title: "Model configuration".to_owned(),
                    details: model_configuration_details(state),
                });
            }
            ModelCommand::SetModel(model) => {
                state.config.model.name = model;
                let _ = events.send(state.settings_event());
                let _ = events.send(RuntimeEvent::Notice {
                    title: "Model updated".to_owned(),
                    details: model_configuration_details(state),
                });
            }
            ModelCommand::SetProvider(provider) => {
                if !is_supported_provider(&provider) {
                    return Err(RuntimeError::InvalidCommand(format!(
                        "supported providers are {}",
                        SUPPORTED_PROVIDERS.join(", ")
                    )));
                }
                state.config.model.protocol = protocol_for_provider(&provider).to_owned();
                state.config.model.provider = provider;
                let _ = events.send(state.settings_event());
                let _ = events.send(RuntimeEvent::Notice {
                    title: "Provider updated".to_owned(),
                    details: model_configuration_details(state),
                });
            }
            ModelCommand::SetApiKey(key) => {
                state.session_api_key = Some(key);
                let _ = events.send(RuntimeEvent::Notice {
                    title: "API key updated".to_owned(),
                    details: vec![
                        "The key is applied only to this Medusa process and is not shown, logged, or written to disk."
                            .to_owned(),
                    ],
                });
            }
        },
        SlashCommand::Effort { effort } => match effort {
            Some(Effort::Auto) => {
                state.config.agent.max_turns = state.base_config.agent.max_turns;
                state.effort = Effort::Auto;
                let _ = events.send(state.settings_event());
            }
            Some(effort) => {
                state.config.agent.max_turns = turns_for_effort(effort);
                state.effort = effort;
                let _ = events.send(state.settings_event());
            }
            None => {
                let _ = events.send(RuntimeEvent::Notice {
                    title: "Effort".to_owned(),
                    details: vec![format!(
                        "{} ({} turn budget)",
                        state.effort.label(),
                        state.config.agent.max_turns
                    )],
                });
            }
        },
        SlashCommand::Verbose { mode } => {
            let next = mode.unwrap_or_else(|| state.verbosity.cycled());
            state.verbosity = next;
            let _ = events.send(state.settings_event());
            let _ = events.send(RuntimeEvent::Notice {
                title: "Verbosity".to_owned(),
                details: vec![format!(
                    "verbosity:{} — tool activity rows are {}.",
                    next.label(),
                    match next {
                        Verbosity::Off => "hidden",
                        Verbosity::New => "limited to the latest row",
                        Verbosity::All => "all shown",
                        Verbosity::Verbose => "all shown with details expanded",
                    }
                )],
            });
        }
        SlashCommand::Skills => {
            let skills = discover_skills(&state.repo);
            let _ = events.send(RuntimeEvent::Notice {
                title: "Available skills".to_owned(),
                details: if skills.is_empty() {
                    vec![
                        "No skills found in .medusa/skills, .claude/skills, or their user equivalents."
                            .to_owned(),
                    ]
                } else {
                    let mut details = vec![
                        "Run /<name> to load a skill for the next prompt, or /<name> <task> to use it immediately."
                            .to_owned(),
                    ];
                    details.extend(skills);
                    details
                },
            });
        }
        SlashCommand::Skill { selector, task } if selector.eq_ignore_ascii_case("recovery") => {
            match recovery_tui::execute_command(&state.repo, task.as_deref()) {
                Ok(Some(receipt)) => {
                    let _ = events.send(RuntimeEvent::RecoveryCompleted(receipt));
                }
                Ok(None) => {
                    for event in recovery::startup_events(&state.repo) {
                        let _ = events.send(event);
                    }
                    let _ = events.send(RuntimeEvent::Notice {
                        title: "Recovery commands".to_owned(),
                        details: vec!["/recovery inspect|resume|verify|abandon or /recovery restore <checkpoint> [--confirm]".to_owned()],
                    });
                }
                Err(error) => {
                    let _ = events.send(RuntimeEvent::Notice {
                        title: "Recovery action failed closed".to_owned(),
                        details: vec![error],
                    });
                }
            }
        }
        SlashCommand::Skill { selector, task } => {
            let skill = load_selected_skill(&state.repo, &selector)?;
            let label = skill.label();
            if let Some(task) = task {
                state.pending_skill = Some(skill);
                let result = run_prompt(
                    state,
                    PromptDraft {
                        text: task,
                        ..PromptDraft::default()
                    },
                    events,
                    cancel,
                    submission,
                    None,
                )
                .map(Some);
                if result.is_err() {
                    state.pending_skill = None;
                }
                return result;
            }
            state.pending_skill = Some(skill);
            let _ = events.send(RuntimeEvent::Notice {
                title: "Skill loaded".to_owned(),
                details: vec![
                    label,
                    "The next prompt will use this skill without persisting its instructions."
                        .to_owned(),
                ],
            });
        }
        SlashCommand::Plan { task } => {
            if task.as_deref().is_some_and(|value| {
                matches!(value.to_ascii_lowercase().as_str(), "off" | "execute")
            }) {
                state.config.agent.mode = state.base_config.agent.mode;
                state.plan_mode = false;
                let _ = events.send(state.settings_event());
                let _ = events.send(RuntimeEvent::Notice {
                    title: "Planning mode off".to_owned(),
                    details: vec!["Subsequent prompts can make changes again.".to_owned()],
                });
            } else {
                state.config.agent.mode = Mode::ReadOnly;
                state.plan_mode = true;
                let _ = events.send(state.settings_event());
                if let Some(task) = task {
                    return run_prompt(
                        state,
                        PromptDraft {
                            text: task,
                            ..PromptDraft::default()
                        },
                        events,
                        cancel,
                        submission,
                        None,
                    )
                    .map(Some);
                }
                let _ = events.send(RuntimeEvent::Notice {
                    title: "Planning mode on".to_owned(),
                    details: vec![
                        "The next prompt will inspect the repository and return a read-only plan. Use /plan off to resume execution."
                            .to_owned(),
                    ],
                });
            }
        }
    }
    Ok(None)
}
