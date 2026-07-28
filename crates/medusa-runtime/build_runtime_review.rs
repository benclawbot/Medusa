fn runtime_review_arm() -> &'static str {
    r#"        SlashCommand::Review { action } => {
            let workspace = crate::review::read_review_workspace(&state.repo)
                .map_err(|error| RuntimeError::agent(error.to_string()))?;
            match action {
                ReviewCommand::Show { filter } => {
                    let mut details = Vec::new();
                    for file in &workspace.snapshot.files {
                        let label = format!("{:?} {:?} {:?}", file.kind, file.origin, file.verification).to_ascii_lowercase();
                        if filter.as_ref().is_some_and(|value| !file.path.contains(value) && !label.contains(&value.to_ascii_lowercase())) { continue; }
                        details.push(format!("{} | {:?} | {:?} | {:?}", file.path, file.kind, file.origin, file.review_state));
                        if let Some(diff) = workspace.files.iter().find(|candidate| candidate.path == file.path) {
                            for hunk in &diff.hunks { details.push(format!("  hunk {} {}", hunk.id, hunk.header)); }
                        }
                    }
                    if details.is_empty() { details.push("No changes match the review filter.".to_owned()); }
                    let _ = events.send(RuntimeEvent::Notice { title: "Repository review".to_owned(), details });
                }
                ReviewCommand::AcceptFile { path } => {
                    crate::review::apply_review_action(&state.repo, medusa_review_model::ReviewActionRequest::AcceptFile { path, expected_snapshot_id: workspace.snapshot.id.clone() }, "tui")
                        .map_err(|error| RuntimeError::agent(error.to_string()))?;
                    let _ = events.send(RuntimeEvent::Notice { title: "Review accepted".to_owned(), details: vec!["File marked accepted; no commit, push, or merge was performed.".to_owned()] });
                }
                ReviewCommand::AcceptTask => {
                    crate::review::apply_review_action(&state.repo, medusa_review_model::ReviewActionRequest::AcceptTask { expected_snapshot_id: workspace.snapshot.id.clone() }, "tui")
                        .map_err(|error| RuntimeError::agent(error.to_string()))?;
                    let _ = events.send(RuntimeEvent::Notice { title: "Task review accepted".to_owned(), details: vec!["All eligible Medusa changes were marked accepted; repository history was not modified.".to_owned()] });
                }
                ReviewCommand::RevertFile { path } => {
                    let file = workspace.snapshot.file(&path).ok_or_else(|| RuntimeError::agent("review file not found"))?;
                    crate::review::apply_review_action(&state.repo, medusa_review_model::ReviewActionRequest::RevertFile { path, expected_snapshot_id: workspace.snapshot.id.clone(), expected_file_fingerprint: file.current_fingerprint.clone() }, "tui")
                        .map_err(|error| RuntimeError::agent(error.to_string()))?;
                    let _ = events.send(RuntimeEvent::Notice { title: "File reverted".to_owned(), details: vec!["The selected Medusa file change was reverted safely.".to_owned()] });
                }
                ReviewCommand::RevertHunk { path, hunk_id } => {
                    let file = workspace.snapshot.file(&path).ok_or_else(|| RuntimeError::agent("review file not found"))?;
                    let hunk = file.hunks.iter().find(|candidate| candidate.id == hunk_id).ok_or_else(|| RuntimeError::agent("review hunk not found"))?;
                    crate::review::apply_review_action(&state.repo, medusa_review_model::ReviewActionRequest::RevertHunk { path, hunk_id, expected_snapshot_id: workspace.snapshot.id.clone(), expected_file_fingerprint: file.current_fingerprint.clone(), expected_hunk_fingerprint: hunk.current_fingerprint.clone() }, "tui")
                        .map_err(|error| RuntimeError::agent(error.to_string()))?;
                    let _ = events.send(RuntimeEvent::Notice { title: "Hunk reverted".to_owned(), details: vec!["The selected Medusa hunk was reverted safely.".to_owned()] });
                }
                ReviewCommand::Export => {
                    let generated_at = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as i64;
                    let export = crate::review::export_review_audit(&state.repo, generated_at)
                        .map_err(|error| RuntimeError::agent(error.to_string()))?;
                    let json = serde_json::to_string_pretty(&export).map_err(|error| RuntimeError::agent(error.to_string()))?;
                    let _ = events.send(RuntimeEvent::Notice { title: "Review audit export".to_owned(), details: json.lines().map(str::to_owned).collect() });
                }
            }
        }
"#
}
