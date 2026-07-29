fn runtime_learning_arm() -> &'static str {
    r#"        SlashCommand::Learning { action } => {
            let snapshot = crate::learning_review::read(&state.repo)
                .map_err(|error| RuntimeError::agent(error.to_string()))?;
            match action {
                LearningCommand::Show { filter } => {
                    let mut details = Vec::new();
                    details.push(format!(
                        "privacy: capture={} user_persistence={} cross_repository={} telemetry={} automatic_proposals={}",
                        snapshot.privacy.capture_enabled,
                        snapshot.privacy.user_persistence_enabled,
                        snapshot.privacy.cross_repository_reuse_enabled,
                        snapshot.privacy.telemetry_enabled,
                        snapshot.privacy.automatic_proposals_enabled,
                    ));
                    for item in &snapshot.items {
                        let searchable = format!("{} {} {} {:?} {:?}", item.id, item.title, item.scope, item.kind, item.state).to_ascii_lowercase();
                        if filter.as_ref().is_some_and(|value| !searchable.contains(&value.to_ascii_lowercase())) { continue; }
                        details.push(format!("{} | {:?} | {:?} | {} | confidence {}", item.id, item.state, item.kind, item.scope, item.confidence_milli));
                        details.push(format!("  learned: {}", item.generalized_rule));
                        details.push(format!("  why: {}", item.root_cause));
                        if let Some(replay) = &item.replay {
                            details.push(format!("  replay: reproduced={} resolved={} regressions={}", replay.reproduced, replay.resolved, replay.regression_count));
                        }
                        if !item.conflicts_with.is_empty() { details.push(format!("  conflicts: {}", item.conflicts_with.iter().cloned().collect::<Vec<_>>().join(", "))); }
                    }
                    if snapshot.items.is_empty() { details.push("No learning proposals are recorded.".to_owned()); }
                    let _ = events.send(RuntimeEvent::Notice { title: "Learning review".to_owned(), details });
                }
                LearningCommand::Privacy => {
                    let preview = crate::learning_review::redaction_preview(&state.repo)
                        .map_err(|error| RuntimeError::agent(error.to_string()))?;
                    let _ = events.send(RuntimeEvent::Notice {
                        title: "Learning privacy".to_owned(),
                        details: vec![
                            format!("Capture: {}", snapshot.privacy.capture_enabled),
                            format!("User-level persistence: {}", snapshot.privacy.user_persistence_enabled),
                            format!("Cross-repository reuse: {}", snapshot.privacy.cross_repository_reuse_enabled),
                            format!("Telemetry: {}", snapshot.privacy.telemetry_enabled),
                            format!("Automatic proposals: {}", snapshot.privacy.automatic_proposals_enabled),
                            format!("Export redaction safe: {}", preview.safe),
                            preview.warnings.join(" "),
                        ],
                    });
                }
                LearningCommand::Export => {
                    let export = crate::learning_review::export(&state.repo)
                        .map_err(|error| RuntimeError::agent(error.to_string()))?;
                    let json = serde_json::to_string_pretty(&export)
                        .map_err(|error| RuntimeError::agent(error.to_string()))?;
                    let _ = events.send(RuntimeEvent::Notice { title: "Learning audit export".to_owned(), details: json.lines().map(str::to_owned).collect() });
                }
                action => {
                    let (id, target) = match action {
                        LearningCommand::Approve { id } => (id, crate::learning_review::LearningReviewState::Approved),
                        LearningCommand::Reject { id } => (id, crate::learning_review::LearningReviewState::Rejected),
                        LearningCommand::Defer { id } => (id, crate::learning_review::LearningReviewState::Deferred),
                        LearningCommand::Validate { id } => (id, crate::learning_review::LearningReviewState::Validated),
                        LearningCommand::Activate { id } => (id, crate::learning_review::LearningReviewState::Active),
                        LearningCommand::Suspend { id } => (id, crate::learning_review::LearningReviewState::Suspended),
                        LearningCommand::Rollback { id } => (id, crate::learning_review::LearningReviewState::RolledBack),
                        LearningCommand::Delete { id } => (id, crate::learning_review::LearningReviewState::Deleted),
                        LearningCommand::Show { .. } | LearningCommand::Privacy | LearningCommand::Export => unreachable!(),
                    };
                    let updated = crate::learning_review::transition(&state.repo, &id, target, snapshot.revision, "tui")
                        .map_err(|error| RuntimeError::agent(error.to_string()))?;
                    let item = updated.items.iter().find(|item| item.id == id)
                        .ok_or_else(|| RuntimeError::agent("updated learning item disappeared"))?;
                    let _ = events.send(RuntimeEvent::Notice {
                        title: "Learning lifecycle updated".to_owned(),
                        details: vec![format!("{} is now {:?} at revision {}", item.id, item.state, updated.revision)],
                    });
                }
            }
        }
"#
}
