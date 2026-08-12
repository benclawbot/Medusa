use std::{fs, process::Command};

fn medusa() -> Command {
    Command::new(env!("CARGO_BIN_EXE_medusa"))
}

#[test]
fn version_and_help_are_available() {
    let version = medusa().arg("--version").output().expect("version");
    assert!(version.status.success());
    assert!(String::from_utf8_lossy(&version.stdout).contains("medusa"));

    let help = medusa().arg("--help").output().expect("help");
    assert!(help.status.success());
    assert!(String::from_utf8_lossy(&help.stdout).contains("Autonomous coding agent"));
}

#[test]
fn bootstrap_and_search_work_on_a_temporary_repository() {
    let directory = tempfile::tempdir().expect("tempdir");
    fs::write(
        directory.path().join("notes.txt"),
        "alpha\nneedle here\nomega\n",
    )
    .expect("fixture");

    let bootstrap = medusa()
        .args([
            "--repo",
            directory.path().to_str().expect("repo"),
            "bootstrap",
        ])
        .output()
        .expect("bootstrap");
    assert!(
        bootstrap.status.success(),
        "{}",
        String::from_utf8_lossy(&bootstrap.stderr)
    );
    assert!(directory.path().join(".medusa").exists());

    let search = medusa()
        .args([
            "--repo",
            directory.path().to_str().expect("repo"),
            "search",
            "needle",
        ])
        .output()
        .expect("search");
    assert!(search.status.success());
    let stdout = String::from_utf8_lossy(&search.stdout);
    assert!(stdout.contains("notes.txt:2:needle here"));
}

#[test]
fn migrate_produces_receipts_and_current_state() {
    let directory = tempfile::tempdir().expect("tempdir");
    let output = medusa()
        .args([
            "--repo",
            directory.path().to_str().expect("repo"),
            "migrate",
        ])
        .output()
        .expect("migrate");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).starts_with('['));
    assert!(directory.path().join(".medusa").exists());
}

#[test]
fn shell_allows_safe_programs_and_rejects_dangerous_ones() {
    let directory = tempfile::tempdir().expect("tempdir");
    let safe = medusa()
        .args([
            "--repo",
            directory.path().to_str().expect("repo"),
            "shell",
            "true",
        ])
        .output()
        .expect("safe shell");
    assert!(
        safe.status.success(),
        "{}",
        String::from_utf8_lossy(&safe.stderr)
    );

    let denied = medusa()
        .args([
            "--repo",
            directory.path().to_str().expect("repo"),
            "shell",
            "rm",
            "anything",
        ])
        .output()
        .expect("denied shell");
    assert!(!denied.status.success());
    assert!(String::from_utf8_lossy(&denied.stderr).contains("hard-denied command: rm"));
}

#[test]
fn shell_reports_nonzero_and_missing_program_failures() {
    let directory = tempfile::tempdir().expect("tempdir");
    let nonzero = medusa()
        .args([
            "--repo",
            directory.path().to_str().expect("repo"),
            "shell",
            "git",
            "definitely-not-a-command",
        ])
        .output()
        .expect("nonzero shell");
    assert!(!nonzero.status.success());
    assert!(String::from_utf8_lossy(&nonzero.stderr).contains("command exited with"));

    let missing = medusa()
        .args([
            "--repo",
            directory.path().to_str().expect("repo"),
            "shell",
            "medusa-command-that-does-not-exist",
        ])
        .output()
        .expect("missing shell");
    assert!(!missing.status.success());
}

#[test]
fn interactive_only_flags_are_rejected_with_subcommands() {
    let directory = tempfile::tempdir().expect("tempdir");
    for flag in [["--prompt", "hello"], ["--resume", "session-1"]] {
        let output = medusa()
            .args([
                "--repo",
                directory.path().to_str().expect("repo"),
                flag[0],
                flag[1],
                "doctor",
            ])
            .output()
            .expect("invalid combination");
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("interactive-only"));
    }
}

#[test]
fn malformed_override_is_rejected_by_clap() {
    let output = medusa()
        .args(["--set", "missing-equals", "doctor"])
        .output()
        .expect("invalid override");
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("expected key=value"));
}

#[test]
fn doctor_emits_checks_and_fails_without_provider_credential() {
    let directory = tempfile::tempdir().expect("tempdir");
    let output = medusa()
        .env_remove("MINIMAX_API_KEY")
        .env("XDG_CONFIG_HOME", directory.path())
        .env("APPDATA", directory.path())
        .args(["--repo", directory.path().to_str().expect("repo"), "doctor"])
        .output()
        .expect("doctor");
    assert!(!output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("provider_credential"));
    assert!(stdout.contains("state_permissions"));
    assert!(String::from_utf8_lossy(&output.stderr).contains("doctor checks failed"));
}

#[test]
fn checkpoint_fails_cleanly_outside_a_git_repository() {
    let directory = tempfile::tempdir().expect("tempdir");
    fs::write(directory.path().join("change.txt"), "change").expect("fixture");
    let output = medusa()
        .args([
            "--repo",
            directory.path().to_str().expect("repo"),
            "checkpoint",
            "test checkpoint",
        ])
        .output()
        .expect("checkpoint");
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("git add -A failed"));
}
