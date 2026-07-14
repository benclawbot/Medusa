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
    fs::create_dir_all(&dir).map_err(|e| io_err("create artifacts dir", e))?;
    let id = Ulid::new();
    let ext = match format {
        OutputFormat::Plain | OutputFormat::JsonLines => "txt",
        OutputFormat::Binary => "bin",
    };
    let path = dir.join(format!("{tool}_{id}.{ext}"));
    let mut file = fs::File::create(&path).map_err(|e| io_err("create artifact", e))?;
    file.write_all(body).map_err(|e| io_err("write artifact", e))?;
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

fn io_err(ctx: &str, e: std::io::Error) -> MedusaError {
    MedusaError::new(
        ErrorCode::ToolExecutionFailed,
        ErrorCategory::Execution,
        format!("{ctx}: {e}"),
    )
}