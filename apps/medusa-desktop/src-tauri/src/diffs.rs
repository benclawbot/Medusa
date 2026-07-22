use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use serde::Serialize;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopDiff {
    pub files: Vec<DesktopDiffFile>,
    pub additions: usize,
    pub deletions: usize,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopDiffFile {
    pub old_path: String,
    pub new_path: String,
    pub status: DesktopDiffStatus,
    pub binary: bool,
    pub additions: usize,
    pub deletions: usize,
    pub hunks: Vec<DesktopDiffHunk>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum DesktopDiffStatus {
    Added,
    Deleted,
    Modified,
    Renamed,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopDiffHunk {
    pub header: String,
    pub lines: Vec<DesktopDiffLine>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopDiffLine {
    pub kind: DesktopDiffLineKind,
    pub old_line: Option<usize>,
    pub new_line: Option<usize>,
    pub text: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum DesktopDiffLineKind {
    Context,
    Addition,
    Deletion,
    Meta,
}

#[tauri::command]
pub fn runtime_read_diff(repo: String) -> Result<DesktopDiff, String> {
    let repo = canonical_repo(&repo)?;
    parse_diff(&diff_output(&repo)?)
}

fn canonical_repo(repo: &str) -> Result<PathBuf, String> {
    let repo = fs::canonicalize(Path::new(repo))
        .map_err(|error| format!("cannot open {repo}: {error}"))?;
    if !repo.is_dir() {
        return Err(format!("{} is not a directory", repo.display()));
    }
    Ok(repo)
}

fn diff_output(repo: &Path) -> Result<String, String> {
    let with_head = run_git_diff(repo, true)?;
    let output = if with_head.status.success() {
        with_head
    } else {
        run_git_diff(repo, false)?
    };
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_owned());
    }
    String::from_utf8(output.stdout).map_err(|_| "git diff output was not UTF-8".to_owned())
}

fn run_git_diff(repo: &Path, include_head: bool) -> Result<std::process::Output, String> {
    let mut command = Command::new("git");
    command.args(["diff", "--no-ext-diff", "--find-renames", "--unified=3"]);
    if include_head {
        command.arg("HEAD");
    }
    command.arg("--").current_dir(repo);
    command
        .output()
        .map_err(|error| format!("cannot run git diff: {error}"))
}

fn parse_diff(source: &str) -> Result<DesktopDiff, String> {
    let mut files = Vec::new();
    let mut current: Option<DesktopDiffFile> = None;
    let mut hunk: Option<DesktopDiffHunk> = None;
    let mut old_line = 0usize;
    let mut new_line = 0usize;

    for line in source.lines() {
        if let Some(paths) = line.strip_prefix("diff --git a/") {
            finish_hunk(&mut current, &mut hunk);
            if let Some(file) = current.take() {
                files.push(file);
            }
            let (old_path, new_path) = paths
                .split_once(" b/")
                .ok_or_else(|| format!("invalid git diff header: {line}"))?;
            current = Some(DesktopDiffFile {
                old_path: old_path.to_owned(),
                new_path: new_path.to_owned(),
                status: DesktopDiffStatus::Modified,
                binary: false,
                additions: 0,
                deletions: 0,
                hunks: Vec::new(),
            });
            continue;
        }

        let Some(file) = current.as_mut() else {
            continue;
        };
        if line.starts_with("new file mode ") {
            file.status = DesktopDiffStatus::Added;
        } else if line.starts_with("deleted file mode ") {
            file.status = DesktopDiffStatus::Deleted;
        } else if let Some(path) = line.strip_prefix("rename from ") {
            file.old_path = path.to_owned();
            file.status = DesktopDiffStatus::Renamed;
        } else if let Some(path) = line.strip_prefix("rename to ") {
            file.new_path = path.to_owned();
            file.status = DesktopDiffStatus::Renamed;
        } else if line.starts_with("Binary files ") || line == "GIT binary patch" {
            file.binary = true;
        } else if line.starts_with("@@ ") {
            finish_hunk(&mut current, &mut hunk);
            let (old, new) = parse_hunk_ranges(line)?;
            old_line = old;
            new_line = new;
            hunk = Some(DesktopDiffHunk {
                header: line.to_owned(),
                lines: Vec::new(),
            });
        } else if let Some(active) = hunk.as_mut() {
            let (kind, old, new, text) = if let Some(text) = line.strip_prefix('+') {
                file.additions += 1;
                let number = new_line;
                new_line += 1;
                (DesktopDiffLineKind::Addition, None, Some(number), text)
            } else if let Some(text) = line.strip_prefix('-') {
                file.deletions += 1;
                let number = old_line;
                old_line += 1;
                (DesktopDiffLineKind::Deletion, Some(number), None, text)
            } else if let Some(text) = line.strip_prefix(' ') {
                let old = old_line;
                let new = new_line;
                old_line += 1;
                new_line += 1;
                (DesktopDiffLineKind::Context, Some(old), Some(new), text)
            } else {
                (DesktopDiffLineKind::Meta, None, None, line)
            };
            active.lines.push(DesktopDiffLine {
                kind,
                old_line: old,
                new_line: new,
                text: text.to_owned(),
            });
        }
    }

    finish_hunk(&mut current, &mut hunk);
    if let Some(file) = current {
        files.push(file);
    }
    let additions = files.iter().map(|file| file.additions).sum();
    let deletions = files.iter().map(|file| file.deletions).sum();
    Ok(DesktopDiff {
        files,
        additions,
        deletions,
    })
}

fn finish_hunk(file: &mut Option<DesktopDiffFile>, hunk: &mut Option<DesktopDiffHunk>) {
    if let (Some(file), Some(hunk)) = (file.as_mut(), hunk.take()) {
        file.hunks.push(hunk);
    }
}

fn parse_hunk_ranges(header: &str) -> Result<(usize, usize), String> {
    let mut fields = header.split_whitespace();
    let _marker = fields.next();
    let old = fields
        .next()
        .ok_or_else(|| format!("invalid hunk header: {header}"))?;
    let new = fields
        .next()
        .ok_or_else(|| format!("invalid hunk header: {header}"))?;
    Ok((range_start(old, '-')?, range_start(new, '+')?))
}

fn range_start(value: &str, prefix: char) -> Result<usize, String> {
    value
        .strip_prefix(prefix)
        .and_then(|value| value.split(',').next())
        .ok_or_else(|| format!("invalid diff range: {value}"))?
        .parse()
        .map_err(|_| format!("invalid diff range: {value}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_multiple_files_and_line_numbers() {
        let source = "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1,2 +1,2 @@\n-old\n+new\n same\ndiff --git a/b.txt b/b.txt\nnew file mode 100644\n--- /dev/null\n+++ b/b.txt\n@@ -0,0 +1 @@\n+created\n";
        let diff = parse_diff(source).expect("parse diff");
        assert_eq!(diff.files.len(), 2);
        assert_eq!(diff.additions, 2);
        assert_eq!(diff.deletions, 1);
        assert!(matches!(diff.files[1].status, DesktopDiffStatus::Added));
        assert_eq!(diff.files[0].hunks[0].lines[0].old_line, Some(1));
        assert_eq!(diff.files[0].hunks[0].lines[1].new_line, Some(1));
    }

    #[test]
    fn recognizes_renames_and_binary_files() {
        let source = "diff --git a/old.png b/new.png\nsimilarity index 100%\nrename from old.png\nrename to new.png\nBinary files a/old.png and b/new.png differ\n";
        let diff = parse_diff(source).expect("parse diff");
        assert!(matches!(diff.files[0].status, DesktopDiffStatus::Renamed));
        assert!(diff.files[0].binary);
    }
}
