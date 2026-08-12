use std::{collections::BTreeMap, fs, io::Write, path::PathBuf};

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use ulid::Ulid;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum OutputFormat {
    Plain,
    JsonLines,
    Binary,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputMode {
    #[default]
    Compact,
    Normal,
    Verbatim,
}

impl OutputMode {
    pub fn parse(value: Option<&str>) -> MedusaResult<Self> {
        match value.unwrap_or("compact") {
            "compact" => Ok(Self::Compact),
            "normal" => Ok(Self::Normal),
            "verbatim" => Ok(Self::Verbatim),
            other => Err(MedusaError::new(
                ErrorCode::InvalidConfiguration,
                ErrorCategory::Validation,
                format!("unsupported output mode: {other}"),
            )),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AdaptedOutput {
    pub mode: OutputMode,
    pub rendered: String,
    pub original_lines: usize,
    pub omitted_lines: usize,
    pub duplicate_lines_removed: usize,
    pub expansion_handle: Option<String>,
}

impl std::fmt::Display for AdaptedOutput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.rendered)?;
        if self.omitted_lines > 0 || self.duplicate_lines_removed > 0 {
            write!(
                f,
                "\n[output-adapter mode={:?}; original_lines={}; omitted_lines={}; duplicate_lines_removed={}",
                self.mode, self.original_lines, self.omitted_lines, self.duplicate_lines_removed
            )?;
            if let Some(handle) = &self.expansion_handle {
                write!(f, "; expansion_handle={handle}")?;
            }
            write!(f, "]")?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EnvelopeConfig {
    pub head_bytes: usize,
    pub tail_bytes: usize,
    pub max_artifact_bytes: usize,
    pub session_root: PathBuf,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OutputEnvelope {
    pub head: String,
    pub tail: String,
    pub line_count: usize,
    pub byte_count: usize,
    pub path: PathBuf,
    pub format: OutputFormat,
}

impl std::fmt::Display for OutputEnvelope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.tail.is_empty() {
            write!(f, "{}", self.head)
        } else {
            write!(
                f,
                "{}\n…\n{}\n({} lines, {} bytes, full body at {})",
                self.head,
                self.tail,
                self.line_count,
                self.byte_count,
                self.path.display()
            )
        }
    }
}

pub fn adapt_text(tool: &str, body: &str, mode: OutputMode) -> AdaptedOutput {
    let cleaned = strip_ansi_and_progress(body);
    let original_lines = cleaned.lines().count();
    if mode == OutputMode::Verbatim {
        return AdaptedOutput {
            mode,
            rendered: cleaned,
            original_lines,
            omitted_lines: 0,
            duplicate_lines_removed: 0,
            expansion_handle: None,
        };
    }

    let (deduplicated, duplicate_lines_removed) = deduplicate_lines(&cleaned);
    let limit = match mode {
        OutputMode::Compact => 80,
        OutputMode::Normal => 240,
        OutputMode::Verbatim => usize::MAX,
    };
    let lines = deduplicated.lines().collect::<Vec<_>>();
    let retained = retain_failures_and_boundaries(&lines, limit);
    let omitted_lines = original_lines
        .saturating_sub(retained.len())
        .saturating_sub(duplicate_lines_removed);
    let expansion_handle = (omitted_lines > 0 || duplicate_lines_removed > 0)
        .then(|| deterministic_handle(tool, body));
    AdaptedOutput {
        mode,
        rendered: retained.join("\n"),
        original_lines,
        omitted_lines,
        duplicate_lines_removed,
        expansion_handle,
    }
}

pub fn adapt_command(
    program: &str,
    args: &[impl AsRef<str>],
    stdout: &[u8],
    stderr: &[u8],
    success: bool,
    mode: OutputMode,
) -> AdaptedOutput {
    let command = format!(
        "command={} {}",
        program,
        args.iter()
            .map(|arg| arg.as_ref())
            .collect::<Vec<_>>()
            .join(" ")
    );
    let stdout = String::from_utf8_lossy(stdout);
    let stderr = String::from_utf8_lossy(stderr);
    let raw = format!("{command}\nstdout:\n{stdout}\nstderr:\n{stderr}");
    if mode == OutputMode::Verbatim {
        return adapt_text("shell_run", &raw, mode);
    }

    let combined = format!("{stdout}\n{stderr}");
    let cleaned = strip_ansi_and_progress(&combined);
    let original_lines = raw.lines().count();
    let (deduplicated, duplicate_lines_removed) = deduplicate_lines(&cleaned);
    let lines = deduplicated.lines().collect::<Vec<_>>();
    let mut retained = (if program == "git" {
        adapt_git_lines(args, &lines, mode)
    } else if success {
        retain_failures_and_boundaries(&lines, if mode == OutputMode::Compact { 40 } else { 160 })
    } else {
        retain_failure_context(
            &lines,
            if mode == OutputMode::Compact {
                120
            } else {
                320
            },
        )
    })
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    retained.insert(0, command);
    retained.insert(
        1,
        format!("status={}", if success { "success" } else { "failure" }),
    );
    let omitted_lines = original_lines
        .saturating_sub(retained.len())
        .saturating_sub(duplicate_lines_removed);
    let expansion_handle = (omitted_lines > 0 || duplicate_lines_removed > 0)
        .then(|| deterministic_handle("shell_run", &raw));
    AdaptedOutput {
        mode,
        rendered: retained.join("\n"),
        original_lines,
        omitted_lines,
        duplicate_lines_removed,
        expansion_handle,
    }
}

pub fn wrap(
    tool: &str,
    body: &[u8],
    format: OutputFormat,
    config: &EnvelopeConfig,
) -> MedusaResult<OutputEnvelope> {
    if body.len() > config.max_artifact_bytes {
        return Err(MedusaError::new(
            ErrorCode::ToolExecutionFailed,
            ErrorCategory::Execution,
            format!(
                "{tool}: output body is {} bytes, exceeds artifact limit {}",
                body.len(),
                config.max_artifact_bytes
            ),
        ));
    }

    let dir = config.session_root.join("artifacts");
    fs::create_dir_all(&dir)?;
    let id = Ulid::new();
    let ext = match format {
        OutputFormat::Plain | OutputFormat::JsonLines => "txt",
        OutputFormat::Binary => "bin",
    };
    let safe_tool = sanitize_tool_name(tool)?;
    let path = dir.join(format!("{safe_tool}_{id}.{ext}"));
    let mut file = fs::File::create(&path)?;
    file.write_all(body)?;
    file.sync_all().ok();

    let text = String::from_utf8_lossy(body);
    let line_count = text.matches('\n').count() + if text.ends_with('\n') { 0 } else { 1 };
    let (head, tail) = split_utf8_boundaries(&text, config.head_bytes, config.tail_bytes);

    Ok(OutputEnvelope {
        head,
        tail,
        line_count,
        byte_count: body.len(),
        path,
        format,
    })
}

fn adapt_git_lines<'a>(
    args: &[impl AsRef<str>],
    lines: &[&'a str],
    mode: OutputMode,
) -> Vec<&'a str> {
    let subcommand = args.first().map(AsRef::as_ref).unwrap_or_default();
    let limit = match (subcommand, mode) {
        ("diff", OutputMode::Compact) => 120,
        ("status" | "log", OutputMode::Compact) => 80,
        (_, OutputMode::Compact) => 100,
        (_, OutputMode::Normal) => 300,
        (_, OutputMode::Verbatim) => usize::MAX,
    };
    retain_failures_and_boundaries(lines, limit)
}

fn retain_failures_and_boundaries<'a>(lines: &[&'a str], limit: usize) -> Vec<&'a str> {
    if lines.len() <= limit {
        return lines.to_vec();
    }
    let failures = lines
        .iter()
        .copied()
        .filter(|line| is_failure_line(line))
        .collect::<Vec<_>>();
    let boundary = limit.saturating_sub(failures.len()).max(2);
    let head = boundary / 2;
    let tail = boundary.saturating_sub(head);
    let mut retained = lines.iter().take(head).copied().collect::<Vec<_>>();
    for failure in failures {
        if !retained.contains(&failure) {
            retained.push(failure);
        }
    }
    for line in lines.iter().rev().take(tail).rev().copied() {
        if !retained.contains(&line) {
            retained.push(line);
        }
    }
    retained
}

fn retain_failure_context<'a>(lines: &[&'a str], limit: usize) -> Vec<&'a str> {
    if lines.len() <= limit {
        return lines.to_vec();
    }
    let mut retained = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        if is_failure_line(line) {
            let start = index.saturating_sub(2);
            let end = (index + 3).min(lines.len());
            for candidate in &lines[start..end] {
                if !retained.contains(candidate) {
                    retained.push(*candidate);
                }
            }
        }
        if retained.len() >= limit {
            break;
        }
    }
    if retained.is_empty() {
        retain_failures_and_boundaries(lines, limit)
    } else {
        retained
    }
}

fn is_failure_line(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    [
        "error", "failed", "failure", "panic", "assert", "denied", "conflict", "timeout",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
}

fn deduplicate_lines(input: &str) -> (String, usize) {
    let mut counts = BTreeMap::<String, usize>::new();
    let mut order = Vec::new();
    for line in input
        .lines()
        .map(str::trim_end)
        .filter(|line| !line.is_empty())
    {
        let entry = counts.entry(line.to_owned()).or_default();
        if *entry == 0 {
            order.push(line.to_owned());
        }
        *entry = entry.saturating_add(1);
    }
    let removed = counts.values().map(|count| count.saturating_sub(1)).sum();
    let rendered = order
        .into_iter()
        .map(|line| {
            let count = counts.get(&line).copied().unwrap_or(1);
            if count > 1 {
                format!("{line} [repeated {count} times]")
            } else {
                line
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    (rendered, removed)
}

fn strip_ansi_and_progress(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(character) = chars.next() {
        if character == '\u{1b}' && chars.peek() == Some(&'[') {
            chars.next();
            for code in chars.by_ref() {
                if code.is_ascii_alphabetic() {
                    break;
                }
            }
            continue;
        }
        if character == '\r' {
            if chars.peek() == Some(&'\n') {
                continue;
            }
            output.push('\n');
            continue;
        }
        output.push(character);
    }
    output
}

fn deterministic_handle(tool: &str, body: &str) -> String {
    let digest = Sha256::digest(format!("{tool}\0{body}").as_bytes());
    format!(
        "{tool}:{}:rerun-with-output_mode=verbatim",
        hex::encode(&digest[..8])
    )
}

fn split_utf8_boundaries(text: &str, head_bytes: usize, tail_bytes: usize) -> (String, String) {
    let total = text.len();
    if total <= head_bytes + tail_bytes + 32 {
        return (text.to_owned(), String::new());
    }
    let head_end = floor_char_boundary(text, head_bytes);
    let tail_start = ceil_char_boundary(text, total.saturating_sub(tail_bytes));
    (text[..head_end].to_owned(), text[tail_start..].to_owned())
}

fn floor_char_boundary(text: &str, mut idx: usize) -> usize {
    if idx >= text.len() {
        return text.len();
    }
    while idx > 0 && !text.is_char_boundary(idx) {
        idx -= 1;
    }
    idx
}

fn ceil_char_boundary(text: &str, mut idx: usize) -> usize {
    if idx >= text.len() {
        return text.len();
    }
    while idx < text.len() && !text.is_char_boundary(idx) {
        idx += 1;
    }
    idx
}

fn sanitize_tool_name(tool: &str) -> MedusaResult<String> {
    if tool.is_empty() {
        return Err(MedusaError::new(
            ErrorCode::InvalidConfiguration,
            ErrorCategory::Validation,
            "tool name must not be empty",
        ));
    }
    let sanitized: String = tool
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    Ok(sanitize_truncate(&sanitized, 64))
}

fn sanitize_truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_owned();
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compact_command_preserves_failures_and_exposes_expansion() {
        let stdout = (0..100)
            .map(|index| format!("ok {index}"))
            .chain(["error: assertion failed".to_owned()])
            .collect::<Vec<_>>()
            .join("\n");
        let adapted = adapt_command(
            "cargo",
            &["test"],
            stdout.as_bytes(),
            b"",
            false,
            OutputMode::Compact,
        );
        assert!(adapted.rendered.contains("error: assertion failed"));
        assert!(adapted.omitted_lines > 0);
        assert!(adapted.expansion_handle.is_some());
    }

    #[test]
    fn duplicate_lines_are_grouped_with_counts() {
        let adapted = adapt_text("search_text", "same\nsame\nother", OutputMode::Compact);
        assert!(adapted.rendered.contains("same [repeated 2 times]"));
        assert_eq!(adapted.duplicate_lines_removed, 1);
    }

    #[test]
    fn verbatim_mode_preserves_all_content() {
        let adapted = adapt_text("fs_read", "one\ntwo\n", OutputMode::Verbatim);
        assert_eq!(adapted.rendered, "one\ntwo\n");
        assert_eq!(adapted.omitted_lines, 0);
        assert!(adapted.expansion_handle.is_none());
    }

    #[test]
    fn ansi_sequences_are_removed() {
        let adapted = adapt_text("shell_run", "\u{1b}[31merror\u{1b}[0m", OutputMode::Compact);
        assert_eq!(adapted.rendered, "error");
    }

    #[test]
    fn sanitize_tool_name_rejects_empty() {
        let err = sanitize_tool_name("").expect_err("empty name must fail");
        assert_eq!(err.code, ErrorCode::InvalidConfiguration);
        assert_eq!(err.category, ErrorCategory::Validation);
    }

    #[test]
    fn sanitize_tool_name_replaces_unsafe_chars() {
        assert_eq!(
            sanitize_tool_name("shell_run").expect("safe input"),
            "shell_run"
        );
        assert_eq!(
            sanitize_tool_name("../../etc/passwd").expect("unsafe input"),
            "______etc_passwd"
        );
        assert_eq!(
            sanitize_tool_name("a b/c.d").expect("mixed input"),
            "a_b_c_d"
        );
    }

    #[test]
    fn sanitize_tool_name_caps_length() {
        let long = "a".repeat(200);
        let out = sanitize_tool_name(&long).expect("long input");
        assert_eq!(out.len(), 64);
        assert!(out.chars().all(|c| c == 'a'));
    }
}
