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
    fs::write(
        temp.path().join("tracked.txt"),
        "one\ntwo\nthree\nfour\n",
    )
    .expect("write");
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
    fs::write(
        repo.path().join("tracked.txt"),
        "ONE\ntwo\nthree\nFOUR\n",
    )
    .expect("changes");
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
    .expect("hunk revert");

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
