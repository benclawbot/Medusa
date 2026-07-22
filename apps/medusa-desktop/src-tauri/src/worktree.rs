use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use serde::Serialize;

#[derive(Debug, Default, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopWorktreeStatus {
    pub branch: Option<String>,
    pub upstream: Option<String>,
    pub ahead: usize,
    pub behind: usize,
    pub detached: bool,
    pub entries: Vec<DesktopWorktreeEntry>,
}

#[derive(Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopWorktreeEntry {
    pub path: String,
    pub original_path: Option<String>,
    pub staged: DesktopWorktreeChange,
    pub unstaged: DesktopWorktreeChange,
    pub untracked: bool,
    pub conflicted: bool,
    pub ignored: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum DesktopWorktreeChange {
    Unmodified,
    Added,
    Modified,
    Deleted,
    Renamed,
    Copied,
    Unmerged,
    Unknown,
}

#[tauri::command]
pub fn runtime_read_worktree(repo: String) -> Result<DesktopWorktreeStatus, String> {
    let repo = canonical_repo(&repo)?;
    let output = Command::new("git")
        .args([
            "status",
            "--porcelain=v2",
            "--branch",
            "--ignored=matching",
            "--untracked-files=all",
        ])
        .current_dir(repo)
        .output()
        .map_err(|error| format!("cannot run git status: {error}"))?;

    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_owned());
    }

    let source = String::from_utf8(output.stdout)
        .map_err(|_| "git status output was not UTF-8".to_owned())?;
    parse_worktree_status(&source)
}

fn canonical_repo(repo: &str) -> Result<PathBuf, String> {
    let repo = fs::canonicalize(Path::new(repo))
        .map_err(|error| format!("cannot open {repo}: {error}"))?;
    if !repo.is_dir() {
        return Err(format!("{} is not a directory", repo.display()));
    }
    Ok(repo)
}

fn parse_worktree_status(source: &str) -> Result<DesktopWorktreeStatus, String> {
    let mut status = DesktopWorktreeStatus::default();
    for line in source.lines() {
        if let Some(value) = line.strip_prefix("# branch.head ") {
            if value == "(detached)" {
                status.detached = true;
            } else {
                status.branch = Some(value.to_owned());
            }
            continue;
        }
        if let Some(value) = line.strip_prefix("# branch.upstream ") {
            status.upstream = Some(value.to_owned());
            continue;
        }
        if let Some(value) = line.strip_prefix("# branch.ab ") {
            let mut parts = value.split_whitespace();
            status.ahead = parse_count(parts.next(), '+', line)?;
            status.behind = parse_count(parts.next(), '-', line)?;
            continue;
        }
        if let Some(path) = line.strip_prefix("? ") {
            status.entries.push(simple_entry(path, true, false));
            continue;
        }
        if let Some(path) = line.strip_prefix("! ") {
            status.entries.push(simple_entry(path, false, true));
            continue;
        }
        if line.starts_with("1 ") || line.starts_with("2 ") || line.starts_with("u ") {
            status.entries.push(parse_tracked_entry(line)?);
        }
    }
    Ok(status)
}

fn parse_count(value: Option<&str>, prefix: char, line: &str) -> Result<usize, String> {
    value
        .and_then(|value| value.strip_prefix(prefix))
        .ok_or_else(|| format!("invalid branch divergence line: {line}"))?
        .parse()
        .map_err(|_| format!("invalid branch divergence line: {line}"))
}

fn simple_entry(path: &str, untracked: bool, ignored: bool) -> DesktopWorktreeEntry {
    DesktopWorktreeEntry {
        path: path.to_owned(),
        original_path: None,
        staged: DesktopWorktreeChange::Unmodified,
        unstaged: DesktopWorktreeChange::Unmodified,
        untracked,
        conflicted: false,
        ignored,
    }
}

fn parse_tracked_entry(line: &str) -> Result<DesktopWorktreeEntry, String> {
    let kind = line.as_bytes()[0] as char;
    let (xy, path_field) = match kind {
        '1' => {
            let fields: Vec<&str> = line.splitn(9, ' ').collect();
            (required_field(&fields, 1, line)?, required_field(&fields, 8, line)?)
        }
        '2' => {
            let fields: Vec<&str> = line.splitn(10, ' ').collect();
            (required_field(&fields, 1, line)?, required_field(&fields, 9, line)?)
        }
        'u' => {
            let fields: Vec<&str> = line.splitn(11, ' ').collect();
            (required_field(&fields, 1, line)?, required_field(&fields, 10, line)?)
        }
        _ => return Err(format!("invalid worktree status record: {line}")),
    };

    let staged = xy
        .chars()
        .next()
        .map(change)
        .unwrap_or(DesktopWorktreeChange::Unknown);
    let unstaged = xy
        .chars()
        .nth(1)
        .map(change)
        .unwrap_or(DesktopWorktreeChange::Unknown);
    let conflicted = kind == 'u'
        || matches!(staged, DesktopWorktreeChange::Unmerged)
        || matches!(unstaged, DesktopWorktreeChange::Unmerged);

    let (path, original_path) = if kind == '2' {
        let (path, original) = path_field
            .split_once('\t')
            .ok_or_else(|| format!("invalid rename status record: {line}"))?;
        (path.to_owned(), Some(original.to_owned()))
    } else {
        (path_field.to_owned(), None)
    };

    Ok(DesktopWorktreeEntry {
        path,
        original_path,
        staged,
        unstaged,
        untracked: false,
        conflicted,
        ignored: false,
    })
}

fn required_field<'a>(fields: &'a [&str], index: usize, line: &str) -> Result<&'a str, String> {
    fields
        .get(index)
        .copied()
        .ok_or_else(|| format!("invalid worktree status record: {line}"))
}

fn change(value: char) -> DesktopWorktreeChange {
    match value {
        '.' => DesktopWorktreeChange::Unmodified,
        'A' => DesktopWorktreeChange::Added,
        'M' => DesktopWorktreeChange::Modified,
        'D' => DesktopWorktreeChange::Deleted,
        'R' => DesktopWorktreeChange::Renamed,
        'C' => DesktopWorktreeChange::Copied,
        'U' => DesktopWorktreeChange::Unmerged,
        _ => DesktopWorktreeChange::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_branch_divergence_and_common_entry_types() {
        let source = "# branch.oid abc123\n# branch.head feature/safe\n# branch.upstream origin/feature/safe\n# branch.ab +2 -1\n1 M. N... 100644 100644 100644 abc abc src/staged.rs\n1 .M N... 100644 100644 100644 abc abc src/unstaged.rs\n? src/new.rs\n! target/generated\n";
        let status = parse_worktree_status(source).expect("parse status");
        assert_eq!(status.branch.as_deref(), Some("feature/safe"));
        assert_eq!(status.upstream.as_deref(), Some("origin/feature/safe"));
        assert_eq!((status.ahead, status.behind), (2, 1));
        assert_eq!(status.entries.len(), 4);
        assert_eq!(status.entries[0].staged, DesktopWorktreeChange::Modified);
        assert_eq!(status.entries[1].unstaged, DesktopWorktreeChange::Modified);
        assert!(status.entries[2].untracked);
        assert!(status.entries[3].ignored);
    }

    #[test]
    fn parses_renames_and_conflicts() {
        let source = "2 R. N... 100644 100644 100644 abc abc R100 src/new.rs\tsrc/old.rs\nu UU N... 100644 100644 100644 100644 abc abc abc src/conflict.rs\n";
        let status = parse_worktree_status(source).expect("parse status");
        assert_eq!(status.entries[0].path, "src/new.rs");
        assert_eq!(status.entries[0].original_path.as_deref(), Some("src/old.rs"));
        assert!(status.entries[1].conflicted);
    }

    #[test]
    fn marks_detached_head() {
        let status = parse_worktree_status("# branch.head (detached)\n").expect("parse status");
        assert!(status.detached);
        assert!(status.branch.is_none());
    }
}
