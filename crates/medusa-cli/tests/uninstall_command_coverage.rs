use std::{fs, process::Command};

fn medusa() -> Command {
    Command::new(env!("CARGO_BIN_EXE_medusa"))
}

fn run(repo: &std::path::Path, args: &[&str]) -> std::process::Output {
    medusa()
        .args(["--repo", repo.to_str().expect("repository path")])
        .args(args)
        .output()
        .expect("medusa uninstall")
}

#[test]
fn wrapper_routes_uninstall_to_the_bounded_report() {
    let repository = tempfile::tempdir().expect("repository");
    let output = run(repository.path(), &["uninstall", "--preview", "--json"]);

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("uninstall JSON report");
    assert_eq!(report["action"], "preview");
    assert_eq!(report["scope"], "runtime");
}

#[test]
fn default_uninstall_removes_ephemeral_cache_but_preserves_durable_state() {
    let repository = tempfile::tempdir().expect("repository");
    fs::create_dir_all(repository.path().join(".medusa/cache")).expect("cache");
    fs::write(repository.path().join(".medusa/cache/value"), "ephemeral").expect("cache");
    fs::create_dir_all(repository.path().join(".medusa/sessions")).expect("sessions");
    fs::write(
        repository.path().join(".medusa/sessions/session.json"),
        "session",
    )
    .expect("session");
    fs::write(
        repository.path().join(".medusa/config.toml"),
        "version = 1\n",
    )
    .expect("config");
    fs::create_dir_all(repository.path().join(".medusa/skills/user")).expect("skills");
    fs::write(
        repository.path().join(".medusa/skills/user/SKILL.md"),
        "user skill",
    )
    .expect("skill");

    let output = run(repository.path(), &["uninstall"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!repository.path().join(".medusa/cache").exists());
    assert!(
        repository
            .path()
            .join(".medusa/sessions/session.json")
            .exists()
    );
    assert!(repository.path().join(".medusa/config.toml").exists());
    assert!(
        repository
            .path()
            .join(".medusa/skills/user/SKILL.md")
            .exists()
    );
}

#[test]
fn purge_requires_an_exact_scope() {
    let repository = tempfile::tempdir().expect("repository");
    let output = run(repository.path(), &["uninstall", "--purge"]);

    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("exact scope"));
}

#[test]
fn preview_is_non_mutating_for_an_explicit_durable_scope() {
    let repository = tempfile::tempdir().expect("repository");
    fs::create_dir_all(repository.path().join(".medusa/skills/user")).expect("skills");
    fs::write(
        repository.path().join(".medusa/skills/user/SKILL.md"),
        "user skill",
    )
    .expect("skill");

    let output = run(
        repository.path(),
        &["uninstall", "--dry-run", "--scope", "skills", "--json"],
    );
    assert!(output.status.success());
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).expect("report");
    assert_eq!(report["action"], "preview");
    assert!(
        !report["preserved"]
            .as_array()
            .expect("preserved")
            .is_empty()
    );
    assert!(
        repository
            .path()
            .join(".medusa/skills/user/SKILL.md")
            .exists()
    );
}

#[test]
fn explicit_purge_data_can_remove_only_the_requested_skill_scope() {
    let repository = tempfile::tempdir().expect("repository");
    fs::create_dir_all(repository.path().join(".medusa/skills/user")).expect("skills");
    fs::write(
        repository.path().join(".medusa/skills/user/SKILL.md"),
        "user skill",
    )
    .expect("skill");
    fs::write(repository.path().join(".medusa/config.toml"), "keep\n").expect("config");

    let output = run(
        repository.path(),
        &["uninstall", "--purge-data", "--scope", "skills"],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!repository.path().join(".medusa/skills").exists());
    assert!(repository.path().join(".medusa/config.toml").exists());
}

#[test]
fn unknown_state_is_reported_and_preserved() {
    let repository = tempfile::tempdir().expect("repository");
    fs::create_dir_all(repository.path().join(".medusa/custom-state")).expect("unknown state");
    fs::write(repository.path().join(".medusa/custom-state/value"), "keep").expect("unknown value");

    let output = run(
        repository.path(),
        &["uninstall", "--purge", "--scope", "runtime", "--json"],
    );
    assert!(output.status.success());
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).expect("report");
    assert!(
        report["protected"]
            .as_array()
            .expect("protected")
            .iter()
            .any(|item| { item["path"] == ".medusa/custom-state" })
    );
    assert!(
        repository
            .path()
            .join(".medusa/custom-state/value")
            .exists()
    );
}

#[cfg(unix)]
#[test]
fn symlinked_state_is_protected_and_partial_failure_is_reported() {
    use std::os::unix::fs::symlink;

    let repository = tempfile::tempdir().expect("repository");
    let outside = tempfile::tempdir().expect("outside");
    let outside_file = outside.path().join("must-survive");
    fs::write(&outside_file, "outside").expect("outside file");
    fs::create_dir_all(repository.path().join(".medusa/cache")).expect("cache");
    symlink(&outside_file, repository.path().join(".medusa/cache/link")).expect("link");
    fs::write(repository.path().join(".medusa/cache/safe"), "remove").expect("safe file");

    let output = run(
        repository.path(),
        &["uninstall", "--purge", "--scope", "runtime", "--json"],
    );
    assert!(!output.status.success());
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).expect("report");
    assert!(!report["removed"].as_array().expect("removed").is_empty());
    assert!(
        report["protected"]
            .as_array()
            .expect("protected")
            .iter()
            .any(|item| { item["path"] == ".medusa/cache/link" })
    );
    assert!(outside_file.exists());
}

#[test]
fn linked_worktree_blocks_cleanup() {
    let repository = tempfile::tempdir().expect("repository");
    fs::write(
        repository.path().join(".git"),
        "gitdir: ../main/.git/worktrees/child\n",
    )
    .expect("worktree marker");
    fs::create_dir_all(repository.path().join(".medusa/cache")).expect("cache");
    fs::write(repository.path().join(".medusa/cache/value"), "keep").expect("cache");

    let output = run(
        repository.path(),
        &["uninstall", "--purge", "--scope", "runtime", "--json"],
    );
    assert!(!output.status.success());
    assert!(repository.path().join(".medusa/cache/value").exists());
}
