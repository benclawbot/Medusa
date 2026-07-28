fn write_generated_review(
    out_dir: &Path,
    review_tests: &Path,
) -> Result<PathBuf, Box<dyn Error>> {
    let mut source = fs::read_to_string("src/review.inc")?.replace("\r\n", "\n");
    replace_once(
        &mut source,
        "    if path.exists() {\n        return Ok(());\n    }",
        "    // Rotate the baseline before every new task so only changes present before that task\n    // are classified as pre-existing user work. Pending-question resumes skip this call.\n    if path.exists() { fs::remove_file(&path).map_err(|error| ReviewWorkflowError::State(error.to_string()))?; }",
    )?;
    replace_once(
        &mut source,
        ".map(|file| (file.path.clone(), file.review_state))",
        ".map(|file| (file.path.clone(), (file.current_fingerprint.clone(), file.review_state)))",
    )?;
    replace_once(
        &mut source,
        "            review_state: prior_states\n                .get(&path)\n                .copied()\n                .unwrap_or(ReviewState::Unreviewed),",
        "            review_state: prior_states\n                .get(&path)\n                .filter(|(fingerprint, _)| fingerprint == &current_fingerprint)\n                .map(|(_, state)| *state)\n                .unwrap_or(ReviewState::Unreviewed),",
    )?;
    source = source.replace(
        "                overlaps_later_edits: false,",
        "                // Without per-write provenance, a tracked-file hunk can contain edits made\n                // by the user while the task was running. Fail closed instead of allowing a\n                // destructive selective revert. Newly added files remain safely revertible.\n                overlaps_later_edits: file.kind != ChangeKind::Added,",
    );
    replace_once(
        &mut source,
        "    match &request {",
        "    // Validate the state transition on a clone before touching the worktree.\n    // This prevents an error such as Accepted -> Reverted from being reported only\n    // after a destructive mutation has already happened.\n    let mut validation_snapshot = state.snapshot.clone();\n    record_authorized_action(\n        &mut validation_snapshot,\n        authorized.clone(),\n        actor,\n        now_unix_ms(),\n        state.snapshot.repository_fingerprint.clone(),\n    )\n    .map_err(|error| ReviewWorkflowError::Rejected(error.to_string()))?;\n\n    match &request {",
    )?;
    source = source.replace("        &authorized,", "        authorized.clone(),");
    source = source.replace(
        "        let patch = String::from_utf8(untracked.stdout)\n            .map_err(|_| ReviewWorkflowError::Git(\"untracked diff was not UTF-8\".to_owned()))?\n            .replace(\"a/dev/null\", \"a/dev/null\");",
        "        let patch = String::from_utf8(untracked.stdout)\n            .map_err(|_| ReviewWorkflowError::Git(\"untracked diff was not UTF-8\".to_owned()))?;",
    );
    source = source.replace(
        "    Ok(String::from_utf8_lossy(&output.stdout).lines().map(str::to_owned).collect())",
        "    Ok(String::from_utf8_lossy(&output.stdout)\n        .lines()\n        .filter(|path| !path.starts_with(\".medusa/review/\"))\n        .map(str::to_owned)\n        .collect())",
    );
    source = source.replace("#[cfg(test)]\n#[path = \"review_tests.rs\"]\nmod tests;", "");
    replace_once(
        &mut source,
        "    state.history.export(generated_at_unix_ms).map_err(Into::into)",
        "    let mut export = state.history.export(generated_at_unix_ms)?;\n    export.resulting_repository_fingerprint = state\n        .history\n        .events\n        .last()\n        .map(|event| event.repository_fingerprint_after.clone());\n    Ok(export)",
    )?;
    source.push_str(&format!(
        "\n#[cfg(test)]\n#[path = \"{}\"]\nmod tests;\n",
        review_tests.display().to_string().replace('\\', "/")
    ));
    let output = out_dir.join("review_generated.rs");
    fs::write(&output, source)?;
    Ok(output)
}

fn write_generated_review_tests(out_dir: &Path) -> Result<PathBuf, Box<dyn Error>> {
    let mut source = fs::read_to_string("src/review_tests.inc")?.replace("\r\n", "\n");
    source.push_str(r#"

#[test]
fn refresh_resets_acceptance_after_file_content_changes() {
    let repo = repository();
    capture_review_baseline(repo.path()).expect("baseline");
    std::fs::write(repo.path().join("tracked.txt"), "changed once\n").expect("edit");
    let first = read_review_workspace(repo.path()).expect("first review");
    let file = first.snapshot.file("tracked.txt").expect("file");
    apply_review_action(repo.path(), ReviewActionRequest::AcceptFile {
        path: file.path.clone(), expected_snapshot_id: first.snapshot.id.clone()
    }, "test").expect("accept");
    std::fs::write(repo.path().join("tracked.txt"), "changed twice\n").expect("edit again");
    let refreshed = read_review_workspace(repo.path()).expect("refresh");
    assert_eq!(refreshed.snapshot.file("tracked.txt").unwrap().review_state, medusa_review_model::ReviewState::Unreviewed);
}

#[test]
fn rejected_transition_does_not_mutate_worktree() {
    let repo = repository();
    capture_review_baseline(repo.path()).expect("baseline");
    std::fs::write(repo.path().join("tracked.txt"), "accepted change\n").expect("edit");
    let first = read_review_workspace(repo.path()).expect("review");
    let file = first.snapshot.file("tracked.txt").expect("file").clone();
    let accepted = apply_review_action(repo.path(), ReviewActionRequest::AcceptFile {
        path: file.path.clone(), expected_snapshot_id: first.snapshot.id.clone()
    }, "test").expect("accept");
    let accepted_file = accepted.snapshot.file("tracked.txt").unwrap();
    let result = apply_review_action(repo.path(), ReviewActionRequest::RevertFile {
        path: "tracked.txt".to_owned(), expected_snapshot_id: accepted.snapshot.id.clone(),
        expected_file_fingerprint: accepted_file.current_fingerprint.clone(),
    }, "test");
    assert!(result.is_err());
    assert_eq!(std::fs::read_to_string(repo.path().join("tracked.txt")).unwrap(), "accepted change\n");
}

#[test]
fn tracked_file_hunks_fail_closed_without_write_provenance() {
    let repo = repository();
    capture_review_baseline(repo.path()).expect("baseline");
    std::fs::write(repo.path().join("tracked.txt"), "agent and user edits\n").expect("edit");
    let workspace = read_review_workspace(repo.path()).expect("review");
    let file = workspace.snapshot.file("tracked.txt").expect("file");
    assert!(file.hunks.iter().all(|hunk| hunk.overlaps_later_edits));
    let result = apply_review_action(repo.path(), ReviewActionRequest::RevertFile {
        path: file.path.clone(),
        expected_snapshot_id: workspace.snapshot.id.clone(),
        expected_file_fingerprint: file.current_fingerprint.clone(),
    }, "test");
    assert!(result.is_err());
    assert_eq!(std::fs::read_to_string(repo.path().join("tracked.txt")).unwrap(), "agent and user edits\n");
}
"#);
    let output = out_dir.join("review_tests_generated.rs");
    fs::write(&output, source)?;
    Ok(output)
}
