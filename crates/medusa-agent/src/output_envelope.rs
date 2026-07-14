use std::{
    fs,
    io::Write,
    path::PathBuf,
};

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use serde::{Deserialize, Serialize};
use ulid::Ulid;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum OutputFormat {
    Plain,
    JsonLines,
    Binary,
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