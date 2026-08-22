use std::path::Path;

use medusa_core::hidden_command;

/// Returns whether `repo` is the root of a Git worktree with a resolvable revision.
///
/// Git normally searches parent directories for a worktree. Repository-scoped Medusa operations
/// must not inherit an ancestor worktree because that would bind indexes and evidence to the wrong
/// repository.
pub(crate) fn is_revisioned_git_repository_root(repo: &Path) -> bool {
    let Ok(expected) = repo.canonicalize() else {
        return false;
    };
    let Ok(output) = hidden_command("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(repo)
        .output()
    else {
        return false;
    };
    if !output.status.success() {
        return false;
    }
    let discovered = String::from_utf8_lossy(&output.stdout);
    let Ok(discovered) = Path::new(discovered.trim()).canonicalize() else {
        return false;
    };
    if discovered != expected {
        return false;
    }

    hidden_command("git")
        .args(["rev-parse", "--verify", "HEAD"])
        .current_dir(repo)
        .output()
        .is_ok_and(|output| output.status.success())
}
