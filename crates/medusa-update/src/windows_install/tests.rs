use std::path::{Path, PathBuf};

use crate::install::Restart;

use super::{acquire_windows_update_lock, helper, sha256_file};

fn write_ready_lock(path: &Path) {
    std::fs::write(
        path,
        b"schema=3\nparent_pid=0\nlock_ready=1\nhelper_pid=0\nhelper_ready=1\n",
    )
    .expect("write helper-ready lock");
}

fn run_helper(script_path: &Path) -> std::process::ExitStatus {
    std::process::Command::new("powershell")
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-File",
        ])
        .arg(script_path)
        .status()
        .expect("run Windows update helper")
}

#[test]
fn windows_helper_contract_restarts_exact_target_and_requires_health() {
    let restart = Restart {
        arguments: vec![
            "--repo".to_owned(),
            r"C:\repo with spaces".to_owned(),
            "--fresh".to_owned(),
        ],
        detached: true,
        sequence_file: Some(PathBuf::from(r"C:\repo\.medusa\update-sequence")),
        rollout_sequence: Some(42),
    };
    let script = helper::windows_health_checked_replace_script(
        4242,
        Path::new(r"C:\bin\medusa-desktop.exe"),
        Path::new(r"C:\bin\medusa-desktop.update-new.exe"),
        Path::new(r"C:\bin\medusa-desktop.previous.exe"),
        Path::new(r"C:\bin\.medusa-update-state"),
        Path::new(r"C:\bin\.medusa-update-health"),
        Path::new(r"C:\bin\.medusa-update.lock"),
        "abc123",
        &restart,
    );
    let stop = script
        .find("Get-TargetProcesses | Stop-Process")
        .expect("stop exact target");
    let replace = script
        .find("Move-Item -LiteralPath $staged -Destination $target")
        .expect("replace executable");
    let restart_index = script.find("$child = Start-Medusa $true").expect("restart");
    let health = script
        .find("Set-Content -LiteralPath $state -Value 'healthy'")
        .expect("health commit");
    assert!(stop < replace);
    assert!(replace < restart_index);
    assert!(restart_index < health);
    assert!(script.contains("MEDUSA_UPDATE_HEALTH_FILE"));
    assert!(script.contains("rolled-back"));
    assert!(script.contains("C:\\repo with spaces"));
    assert!(script.contains("update-sequence"));
    assert!(script.contains("'42'"));
    assert!(!script.contains("--version"));
    assert!(!script.contains("Get-Process -Name 'medusa'"));
}

#[test]
fn windows_helper_commits_only_after_replacement_health_ack() {
    let workspace = tempfile::tempdir().expect("tempdir");
    let root = workspace.path();
    let target = root.join("medusa-desktop.exe");
    let staged = root.join("medusa-desktop.update-new.exe");
    let backup = root.join("medusa-desktop.previous.exe");
    let state = root.join(".medusa-update-state");
    let health = root.join(".medusa-update-health");
    let lock = root.join(".medusa-update.lock");
    let sequence = root.join("update-sequence");
    let helper_path = root.join("medusa-desktop.update.ps1");

    let comspec = PathBuf::from(std::env::var_os("ComSpec").expect("ComSpec"));
    let system_root = PathBuf::from(std::env::var_os("SystemRoot").expect("SystemRoot"));
    let original = system_root.join("System32").join("where.exe");
    std::fs::copy(&original, &target).expect("seed previous target");
    std::fs::copy(&comspec, &staged).expect("stage replacement");
    write_ready_lock(&lock);

    let restart = Restart {
        arguments: vec![
            "/C".to_owned(),
            "echo healthy>%MEDUSA_UPDATE_HEALTH_FILE%".to_owned(),
        ],
        detached: true,
        sequence_file: Some(sequence.clone()),
        rollout_sequence: Some(42),
    };
    let expected_hash = sha256_file(&staged).expect("staged hash");
    let script = helper::windows_health_checked_replace_script(
        0,
        &target,
        &staged,
        &backup,
        &state,
        &health,
        &lock,
        &expected_hash,
        &restart,
    );
    std::fs::write(&helper_path, script).expect("write helper");

    let status = run_helper(&helper_path);
    assert!(status.success());
    assert_eq!(
        std::fs::read_to_string(&state).expect("read state").trim(),
        "healthy"
    );
    assert!(
        std::fs::read_to_string(&health)
            .expect("read health")
            .contains("healthy")
    );
    assert_eq!(
        std::fs::read_to_string(&sequence)
            .expect("read rollout sequence")
            .trim(),
        "42"
    );
    assert_eq!(sha256_file(&target).expect("target hash"), expected_hash);
    assert!(!backup.exists());
    assert!(!lock.exists());
}

#[test]
fn windows_helper_rolls_back_when_replacement_exits_without_health() {
    let workspace = tempfile::tempdir().expect("tempdir");
    let root = workspace.path();
    let target = root.join("medusa-desktop.exe");
    let staged = root.join("medusa-desktop.update-new.exe");
    let backup = root.join("medusa-desktop.previous.exe");
    let state = root.join(".medusa-update-state");
    let health = root.join(".medusa-update-health");
    let lock = root.join(".medusa-update.lock");
    let sequence = root.join("update-sequence");
    let helper_path = root.join("medusa-desktop.update.ps1");

    let comspec = PathBuf::from(std::env::var_os("ComSpec").expect("ComSpec"));
    let system_root = PathBuf::from(std::env::var_os("SystemRoot").expect("SystemRoot"));
    let original = system_root.join("System32").join("where.exe");
    std::fs::copy(&original, &target).expect("seed previous target");
    let original_hash = sha256_file(&target).expect("original hash");
    std::fs::copy(&comspec, &staged).expect("stage replacement");
    write_ready_lock(&lock);

    let restart = Restart {
        arguments: vec!["/C".to_owned(), "exit 7".to_owned()],
        detached: true,
        sequence_file: Some(sequence.clone()),
        rollout_sequence: Some(42),
    };
    let expected_hash = sha256_file(&staged).expect("staged hash");
    let script = helper::windows_health_checked_replace_script(
        0,
        &target,
        &staged,
        &backup,
        &state,
        &health,
        &lock,
        &expected_hash,
        &restart,
    );
    std::fs::write(&helper_path, script).expect("write helper");

    let status = run_helper(&helper_path);
    assert!(!status.success());
    assert_eq!(
        std::fs::read_to_string(&state).expect("read state").trim(),
        "rolled-back"
    );
    assert_eq!(sha256_file(&target).expect("restored hash"), original_hash);
    assert!(!sequence.exists());
    assert!(!backup.exists());
    assert!(!lock.exists());
}

#[test]
fn windows_lock_rejects_live_owner_and_reclaims_stale_owner() {
    let workspace = tempfile::tempdir().expect("tempdir");
    let lock = workspace.path().join(".medusa-update.lock");
    let live =
        acquire_windows_update_lock(&lock, std::process::id()).expect("acquire live update lock");
    let error = acquire_windows_update_lock(&lock, std::process::id())
        .expect_err("second live update must be rejected");
    assert!(error.to_string().contains("already staged"));
    drop(live);
    std::fs::remove_file(&lock).expect("remove live lock");

    std::fs::write(
        &lock,
        b"schema=3\nparent_pid=4294967294\nlock_ready=1\nhelper_ready=1\n",
    )
    .expect("write stale lock");
    let reclaimed =
        acquire_windows_update_lock(&lock, std::process::id()).expect("reclaim stale lock");
    drop(reclaimed);
    std::fs::remove_file(&lock).expect("remove reclaimed lock");
}
