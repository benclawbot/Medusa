use std::fs;

use medusa_daemon::DaemonPaths;

#[test]
fn daemon_paths_are_workspace_scoped_without_requiring_git() {
    let root = tempfile::tempdir().expect("plain workspace");
    fs::write(root.path().join("notes.md"), "research notes\n").expect("workspace file");
    assert!(!root.path().join(".git").exists());

    let paths = DaemonPaths::for_repo(root.path());

    assert_eq!(paths.repo, root.path());
    assert_eq!(paths.directory, root.path().join(".medusa/daemon"));
    assert_eq!(paths.state, root.path().join(".medusa/daemon/jobs.json"));
}
