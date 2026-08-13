#!/usr/bin/env python3
"""Fail closed when a user surface reintroduces a Git-only workspace gate."""

from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]

CHECKS = {
    "CLI": (
        "crates/medusa-cli/src/main.rs",
        [
            "let repo = repository_path(&cli.repo);",
            "TuiOptions::for_repo(repo)",
            "RuntimeController::start_with_config(repo.clone(), config)",
            "telegram_command::run(&repo, *args)",
        ],
    ),
    "TUI": (
        "crates/medusa-tui/src/lib.rs",
        [
            "pub fn for_repo(repo: impl Into<PathBuf>) -> Self",
            'self.repo.join(".medusa/daemon/medusa.sock")',
        ],
    ),
    "Desktop": (
        "apps/medusa-desktop/src-tauri/src/runtime.rs",
        [
            "let runtime_repo = canonical_directory(Path::new(&repo))?;",
            '.join("general-chat")',
            "DaemonSupervisor::new(&repo, launch)",
        ],
    ),
    "Daemon": (
        "crates/medusa-daemon/src/paths.rs",
        [
            'let directory = repo.join(".medusa/daemon");',
        ],
    ),
    "Telegram": (
        "crates/medusa-cli/src/telegram_command.rs",
        [
            'fs::create_dir_all(repo.join(".medusa/telegram"))?;',
            "DaemonSupervisor::new(repo, launch)",
        ],
    ),
}

# These entrypoint files may contain Git-specific optional features, but workspace startup itself
# must never reject a directory solely because it has no .git metadata.
FORBIDDEN_STARTUP_PATTERNS = (
    "repository must be a git repository",
    "workspace must be a git repository",
    "require_git_repository",
    "require_git_repo",
)


def main() -> int:
    failures: list[str] = []
    for surface, (relative, required) in CHECKS.items():
        text = (ROOT / relative).read_text(encoding="utf-8")
        for needle in required:
            if needle not in text:
                failures.append(f"{surface}: missing workspace entrypoint invariant {needle!r} in {relative}")
        lowered = text.lower()
        for forbidden in FORBIDDEN_STARTUP_PATTERNS:
            if forbidden in lowered:
                failures.append(f"{surface}: Git-only startup gate {forbidden!r} found in {relative}")

    workspace_doc = (ROOT / "docs/WORKSPACES.md").read_text(encoding="utf-8")
    for term in ("CLI", "TUI", "Desktop", "Daemon", "Telegram", "ordinary directory", "Git"):
        if term not in workspace_doc:
            failures.append(f"docs/WORKSPACES.md does not document {term!r}")

    if failures:
        for failure in failures:
            print(f"workspace-surface-error: {failure}")
        return 1
    print("workspace-surfaces-ok")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
