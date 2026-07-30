use std::{fs, path::Path, process::Command};

use medusa_workers::WorkerManager;

fn git(repo: &Path, args: &[&str]) {
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
}

#[test]
fn cleanup_removes_ignored_runtime_state_without_touching_product_or_tracked_files() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let repo = directory.path().join("repo");
    let worktrees = directory.path().join("worktrees");
    fs::create_dir(&repo).expect("repository");
    git(&repo, &["init", "-b", "main"]);
    git(&repo, &["config", "user.name", "Medusa Test"]);
    git(&repo, &["config", "user.email", "medusa@example.invalid"]);
    fs::write(repo.join(".gitignore"), ".medusa/\n").expect("gitignore");
    fs::write(repo.join("base.txt"), "base\n").expect("baseline");
    fs::create_dir_all(repo.join(".medusa")).expect("runtime directory");
    fs::write(repo.join(".medusa/tracked.txt"), "tracked\n").expect("tracked runtime file");
    git(&repo, &["add", ".gitignore", "base.txt"]);
    git(&repo, &["add", "-f", ".medusa/tracked.txt"]);
    git(&repo, &["commit", "-m", "baseline"]);

    let manager = WorkerManager::new(&repo, &worktrees).expect("worker manager");
    let worker = manager.create_worker("runtime-cleanup").expect("worker");
    fs::write(
        worker.worktree.join(".medusa/REPOSITORY_MAP.md"),
        "generated\n",
    )
    .expect("generated runtime file");
    fs::write(worker.worktree.join("product.txt"), "product\n").expect("product edit");

    manager
        .discard_untracked_runtime_state(&worker)
        .expect("discard runtime state");

    assert!(!worker.worktree.join(".medusa/REPOSITORY_MAP.md").exists());
    assert_eq!(
        fs::read_to_string(worker.worktree.join(".medusa/tracked.txt")).expect("tracked file"),
        "tracked\n"
    );
    assert_eq!(
        fs::read_to_string(worker.worktree.join("product.txt")).expect("product file"),
        "product\n"
    );

    manager.cleanup(&[worker]).expect("cleanup worker");
}
