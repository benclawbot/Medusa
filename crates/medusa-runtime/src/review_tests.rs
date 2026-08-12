use std::process::Command;

use medusa_review_model::ReviewActionRequest;

use super::*;

fn git(repo: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .expect("git");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn repository() -> tempfile::TempDir {
    let temp = tempfile::tempdir().expect("temp repo");
    git(temp.path(), &["init"]);
    git(temp.path(), &["config", "user.email", "test@example.com"]);
    git(temp.path(), &["config", "user.name", "Test"]);
    fs::write(temp.path().join("tracked.txt"), "one\ntwo\nthree\nfour\n").expect("write");
    git(temp.path(), &["add", "."]);
    git(temp.path(), &["commit", "-m", "initial"]);
    temp
}

#[test]
fn parses_renames_binary_and_multiple_hunks() {
    let source = "diff --git a/old.rs b/new.rs\nsimilarity index 90%\nrename from old.rs\nrename to new.rs\n--- a/old.rs\n+++ b/new.rs\n@@ -1 +1 @@\n-old\n+new\n@@ -4 +4 @@\n-a\n+b\n";
    let files = parse_diff(source).expect("parse");
    assert_eq!(files[0].kind, ChangeKind::Renamed);
    assert_eq!(files[0].previous_path.as_deref(), Some("old.rs"));
    assert_eq!(files[0].hunks.len(), 2);
}

#[test]
fn generated_and_policy_sensitive_paths_are_classified() {
    assert!(generated_path("target/debug/app"));
    assert!(generated_path("Cargo.lock"));
    assert!(policy_sensitive_path(".github/workflows/ci.yml"));
    assert!(policy_sensitive_path("crates/auth/src/lib.rs"));
}

#[test]
fn baseline_preserves_preexisting_tracked_and_untracked_changes() {
    let repo = repository();
    fs::write(repo.path().join("tracked.txt"), "user change\n").expect("user change");
    fs::write(repo.path().join("notes.tmp"), "user note\n").expect("user note");
    capture_review_baseline(repo.path()).expect("baseline");
    fs::write(repo.path().join("medusa.txt"), "agent\n").expect("agent file");
    let workspace = read_review_workspace(repo.path()).expect("workspace");
    let by_path = workspace
        .snapshot
        .files
        .iter()
        .map(|file| (file.path.as_str(), file.origin))
        .collect::<BTreeMap<_, _>>();
    assert_eq!(by_path["tracked.txt"], ChangeOrigin::PreExistingUser);
    assert_eq!(by_path["notes.tmp"], ChangeOrigin::PreExistingUser);
    assert_eq!(by_path["medusa.txt"], ChangeOrigin::Medusa);
}

#[test]
fn file_revert_removes_medusa_file_without_touching_preexisting_change() {
    let repo = repository();
    fs::write(repo.path().join("tracked.txt"), "user change\n").expect("user change");
    capture_review_baseline(repo.path()).expect("baseline");
    fs::write(repo.path().join("medusa.txt"), "agent\n").expect("agent file");
    let workspace = read_review_workspace(repo.path()).expect("workspace");
    let file = workspace.snapshot.file("medusa.txt").expect("file");
    apply_review_action(
        repo.path(),
        ReviewActionRequest::RevertFile {
            path: "medusa.txt".into(),
            expected_snapshot_id: workspace.snapshot.id.clone(),
            expected_file_fingerprint: file.current_fingerprint.clone(),
        },
        "test",
    )
    .expect("revert");
    assert!(!repo.path().join("medusa.txt").exists());
    assert_eq!(
        fs::read_to_string(repo.path().join("tracked.txt")).unwrap(),
        "user change\n"
    );
}

#[test]
fn hunk_revert_and_drift_detection_fail_closed() {
    let repo = repository();
    capture_review_baseline(repo.path()).expect("baseline");
    fs::write(repo.path().join("tracked.txt"), "ONE\ntwo\nthree\nFOUR\n").expect("changes");
    let workspace = read_review_workspace(repo.path()).expect("workspace");
    let file = workspace.snapshot.file("tracked.txt").expect("file");
    let hunk = file.hunks.first().expect("hunk");
    apply_review_action(
        repo.path(),
        ReviewActionRequest::RevertHunk {
            path: "tracked.txt".into(),
            hunk_id: hunk.id.clone(),
            expected_snapshot_id: workspace.snapshot.id.clone(),
            expected_file_fingerprint: file.current_fingerprint.clone(),
            expected_hunk_fingerprint: hunk.current_fingerprint.clone(),
        },
        "test",
    )
    .expect_err("tracked hunk revert must fail closed");

    fs::write(repo.path().join("tracked.txt"), "later edit\n").expect("drift");
    let stale = apply_review_action(
        repo.path(),
        ReviewActionRequest::AcceptFile {
            path: "tracked.txt".into(),
            expected_snapshot_id: workspace.snapshot.id,
        },
        "test",
    );
    assert!(matches!(stale, Err(ReviewWorkflowError::StaleState)));
}

#[test]
fn binary_rename_and_generated_actions_are_blocked_by_model() {
    let repo = repository();
    capture_review_baseline(repo.path()).expect("baseline");
    git(repo.path(), &["mv", "tracked.txt", "renamed.txt"]);
    fs::create_dir_all(repo.path().join("generated")).unwrap();
    fs::write(repo.path().join("generated/out.rs"), "generated\n").unwrap();
    fs::write(repo.path().join("binary.bin"), [0_u8, 159, 146, 150]).unwrap();
    let workspace = read_review_workspace(repo.path()).expect("workspace");
    let rename = workspace.snapshot.file("renamed.txt").expect("rename");
    let request = ReviewActionRequest::RevertFile {
        path: rename.path.clone(),
        expected_snapshot_id: workspace.snapshot.id.clone(),
        expected_file_fingerprint: rename.current_fingerprint.clone(),
    };
    assert!(workspace.snapshot.authorize(request).is_err());
    assert_eq!(
        workspace.snapshot.file("generated/out.rs").unwrap().origin,
        ChangeOrigin::Generated
    );
    assert!(workspace.snapshot.file("binary.bin").unwrap().binary);
}

#[test]
fn baseline_allows_a_plain_non_git_directory() {
    let workspace = tempfile::tempdir().expect("workspace");
    fs::write(workspace.path().join("notes.txt"), "hello\n").expect("write");

    capture_review_baseline(workspace.path()).expect("baseline");

    let baseline: ReviewBaseline =
        read_json(&workspace.path().join(REVIEW_DIR).join(BASELINE_FILE)).expect("read baseline");
    assert!(baseline.paths.is_empty());
    assert_eq!(baseline.repository_fingerprint, fingerprint(b""));
}

#[test]
fn unborn_git_repository_uses_current_worktree_content() {
    let repo = tempfile::tempdir().expect("repo");
    git(repo.path(), &["init"]);
    fs::write(repo.path().join("first.txt"), "staged\n").expect("write staged");
    git(repo.path(), &["add", "first.txt"]);
    fs::write(repo.path().join("first.txt"), "worktree\n").expect("write worktree");

    capture_review_baseline(repo.path()).expect("baseline");
    let workspace = read_review_workspace(repo.path()).expect("workspace");
    let file = workspace.snapshot.file("first.txt").expect("first file");
    let diff = workspace
        .files
        .iter()
        .find(|file| file.path == "first.txt")
        .expect("first diff");

    assert_eq!(file.origin, ChangeOrigin::PreExistingUser);
    assert_eq!(file.current_fingerprint, fingerprint(diff.patch.as_bytes()));
    assert!(diff.patch.contains("+worktree"));
    assert!(!diff.patch.contains("+staged"));
    assert!(
        workspace
            .snapshot
            .file(".medusa/review/baseline.json")
            .is_none()
    );
}

#[test]
fn unborn_git_repository_reverts_a_staged_medusa_file() {
    let repo = tempfile::tempdir().expect("repo");
    git(repo.path(), &["init"]);
    capture_review_baseline(repo.path()).expect("baseline");

    fs::write(repo.path().join("medusa.txt"), "agent\n").expect("agent file");
    git(repo.path(), &["add", "medusa.txt"]);
    let workspace = read_review_workspace(repo.path()).expect("workspace");
    let file = workspace.snapshot.file("medusa.txt").expect("medusa file");

    apply_review_action(
        repo.path(),
        ReviewActionRequest::RevertFile {
            path: "medusa.txt".into(),
            expected_snapshot_id: workspace.snapshot.id.clone(),
            expected_file_fingerprint: file.current_fingerprint.clone(),
        },
        "test",
    )
    .expect("revert");

    assert!(!repo.path().join("medusa.txt").exists());
    assert!(
        !Command::new("git")
            .args(["ls-files", "--error-unmatch", "--", "medusa.txt"])
            .current_dir(repo.path())
            .output()
            .expect("git ls-files")
            .status
            .success()
    );
}

#[test]
fn refresh_resets_acceptance_after_file_content_changes() {
    let repo = repository();
    capture_review_baseline(repo.path()).expect("baseline");
    std::fs::write(repo.path().join("tracked.txt"), "changed once\n").expect("edit");
    let first = read_review_workspace(repo.path()).expect("first review");
    let file = first.snapshot.file("tracked.txt").expect("file");
    apply_review_action(
        repo.path(),
        ReviewActionRequest::AcceptFile {
            path: file.path.clone(),
            expected_snapshot_id: first.snapshot.id.clone(),
        },
        "test",
    )
    .expect("accept");
    std::fs::write(repo.path().join("tracked.txt"), "changed twice\n").expect("edit again");
    let refreshed = read_review_workspace(repo.path()).expect("refresh");
    assert_eq!(
        refreshed.snapshot.file("tracked.txt").unwrap().review_state,
        medusa_review_model::ReviewState::Unreviewed
    );
}

#[test]
fn rejected_transition_does_not_mutate_worktree() {
    let repo = repository();
    capture_review_baseline(repo.path()).expect("baseline");
    std::fs::write(repo.path().join("tracked.txt"), "accepted change\n").expect("edit");
    let first = read_review_workspace(repo.path()).expect("review");
    let file = first.snapshot.file("tracked.txt").expect("file").clone();
    let accepted = apply_review_action(
        repo.path(),
        ReviewActionRequest::AcceptFile {
            path: file.path.clone(),
            expected_snapshot_id: first.snapshot.id.clone(),
        },
        "test",
    )
    .expect("accept");
    let accepted_file = accepted.snapshot.file("tracked.txt").unwrap();
    let result = apply_review_action(
        repo.path(),
        ReviewActionRequest::RevertFile {
            path: "tracked.txt".to_owned(),
            expected_snapshot_id: accepted.snapshot.id.clone(),
            expected_file_fingerprint: accepted_file.current_fingerprint.clone(),
        },
        "test",
    );
    assert!(result.is_err());
    assert_eq!(
        std::fs::read_to_string(repo.path().join("tracked.txt")).unwrap(),
        "accepted change\n"
    );
}

#[test]
fn tracked_file_hunks_fail_closed_without_write_provenance() {
    let repo = repository();
    capture_review_baseline(repo.path()).expect("baseline");
    std::fs::write(repo.path().join("tracked.txt"), "agent and user edits\n").expect("edit");
    let workspace = read_review_workspace(repo.path()).expect("review");
    let file = workspace.snapshot.file("tracked.txt").expect("file");
    assert!(file.hunks.iter().all(|hunk| hunk.overlaps_later_edits));
    let result = apply_review_action(
        repo.path(),
        ReviewActionRequest::RevertFile {
            path: file.path.clone(),
            expected_snapshot_id: workspace.snapshot.id.clone(),
            expected_file_fingerprint: file.current_fingerprint.clone(),
        },
        "test",
    );
    assert!(result.is_err());
    assert_eq!(
        std::fs::read_to_string(repo.path().join("tracked.txt")).unwrap(),
        "agent and user edits\n"
    );
}
