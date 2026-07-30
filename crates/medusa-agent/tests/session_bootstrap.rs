use std::{fs, path::Path, process::Command};

use medusa_agent::bootstrap;

fn git(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .expect("git command");
    assert!(
        output.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
fn bootstrap_keeps_generated_repository_map_inside_runtime_state() {
    let directory = tempfile::tempdir().expect("temporary repository");
    let repo = directory.path();
    git(repo, &["init", "-b", "main"]);
    git(repo, &["config", "user.name", "Medusa Test"]);
    git(repo, &["config", "user.email", "medusa@example.invalid"]);
    fs::write(repo.join("tracked.txt"), "baseline\n").expect("baseline file");
    git(repo, &["add", "tracked.txt"]);
    git(repo, &["commit", "-m", "baseline"]);

    bootstrap(repo).expect("session bootstrap");

    assert!(repo.join(".medusa/REPOSITORY_MAP.md").is_file());
    assert!(!repo.join("REPOSITORY_MAP.md").exists());

    let status = git(repo, &["status", "--porcelain", "--untracked-files=all"]);
    assert!(
        status.lines().all(|line| {
            let path = line.get(3..).unwrap_or_default().trim_matches('"');
            line.starts_with("?? ") && (path == ".medusa" || path.starts_with(".medusa/"))
        }),
        "bootstrap dirtied product paths: {status}"
    );
}
