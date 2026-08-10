//! Contained execution backend for the persistent analysis workspace.
//!
//! The language-neutral workspace remains authoritative for state and provenance. This backend
//! only accelerates bounded reductions with a fixed host-owned Python program. User-supplied code
//! is never evaluated. The containment root is the per-session analysis directory, so the
//! authoritative repository is not a writable sandbox mount.

use std::{
    path::{Path, PathBuf},
    time::Instant,
};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::{
    RuntimeController, RuntimeError,
    analysis_workspace::{
        AnalysisArtifactRef, AnalysisOperation, AnalysisProvenance, AnalysisResult, AnalysisValue,
    },
};

const MAX_CONTAINED_INPUT_BYTES: u64 = 64 * 1024 * 1024;
const MAX_CONTAINED_OUTPUT_BYTES: usize = 16 * 1024;
const MAX_CONTAINED_LINES: usize = 128;

const PYTHON_REDUCER: &str = r#"
import json, sys
path = sys.argv[1]
op = json.loads(sys.argv[2])
with open(path, "rb") as handle:
    data = handle.read()
kind = op["kind"]
truncated = False
if kind == "byte_count":
    value = {"kind": "integer", "value": len(data)}
elif kind == "line_count":
    count = data.count(b"\n") + (1 if data and not data.endswith(b"\n") else 0)
    value = {"kind": "integer", "value": count}
else:
    text = data.decode("utf-8")
    if kind == "utf8_contains":
        value = {"kind": "bool", "value": op["needle"] in text}
    elif kind == "matching_lines":
        limit = min(int(op["limit"]), 128)
        selected = []
        used = 0
        for line in text.splitlines():
            if op["needle"] not in line:
                continue
            encoded = line.encode("utf-8")
            if len(selected) >= limit or used + len(encoded) > 16384:
                truncated = True
                break
            selected.append(line)
            used += len(encoded)
        value = {"kind": "string_list", "value": selected}
    elif kind == "head_lines":
        limit = min(int(op["limit"]), 128)
        selected = []
        used = 0
        for line in text.splitlines():
            encoded = line.encode("utf-8")
            if len(selected) >= limit or used + len(encoded) > 16384:
                truncated = True
                break
            selected.append(line)
            used += len(encoded)
        value = {"kind": "string_list", "value": selected}
    else:
        raise ValueError("unsupported analysis operation")
print(json.dumps({"value": value, "truncated": truncated}, separators=(",", ":")))
"#;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AnalysisWorkspaceCapabilities {
    pub backend: String,
    pub arbitrary_user_code: bool,
    pub ambient_network: bool,
    pub ambient_credentials: bool,
    pub primary_repository_write: bool,
    pub direct_provider_client: bool,
    pub max_input_bytes: u64,
    pub max_output_bytes: usize,
    pub max_result_lines: usize,
    pub process_limit: usize,
    pub cancellation: String,
    pub containment_authority: String,
    pub delegation_authority: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AnalysisExecutionMetrics {
    pub input_bytes: u64,
    pub output_bytes: usize,
    pub elapsed_millis: u128,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ContainedAnalysisResult {
    pub result: AnalysisResult,
    pub metrics: AnalysisExecutionMetrics,
    pub backend: String,
}

impl RuntimeController {
    #[must_use]
    pub fn analysis_workspace_capabilities(&self) -> AnalysisWorkspaceCapabilities {
        AnalysisWorkspaceCapabilities {
            backend: "contained_fixed_python_reducer".to_owned(),
            arbitrary_user_code: false,
            ambient_network: false,
            ambient_credentials: false,
            primary_repository_write: false,
            direct_provider_client: false,
            max_input_bytes: MAX_CONTAINED_INPUT_BYTES,
            max_output_bytes: MAX_CONTAINED_OUTPUT_BYTES,
            max_result_lines: MAX_CONTAINED_LINES,
            process_limit: 1,
            cancellation: "runtime_atomic_cancel_to_process_tree".to_owned(),
            containment_authority: "medusa-agent/policy -> medusa-process-containment".to_owned(),
            delegation_authority: "medusa-runtime/team-control".to_owned(),
        }
    }

    /// Executes one bounded reduction in Medusa's existing process containment. The helper program
    /// is host-owned and fixed; callers choose only a typed operation and a verified artifact.
    pub fn analysis_reduce_contained(
        &self,
        session_id: &str,
        artifact: &AnalysisArtifactRef,
        operation: AnalysisOperation,
    ) -> Result<ContainedAnalysisResult, RuntimeError> {
        if artifact.size_bytes > MAX_CONTAINED_INPUT_BYTES {
            return Err(RuntimeError::InvalidCommand(
                "contained analysis input exceeds the configured byte limit".to_owned(),
            ));
        }
        let root = analysis_root(&self.repo, session_id);
        let artifact_path = PathBuf::from(&artifact.artifact_path);
        let expected_artifacts = root.join("artifacts");
        if artifact_path.parent() != Some(expected_artifacts.as_path()) {
            return Err(RuntimeError::InvalidCommand(
                "contained analysis artifact is outside the session workspace".to_owned(),
            ));
        }
        let bytes = std::fs::read(&artifact_path).map_err(RuntimeError::agent)?;
        if bytes.len() as u64 != artifact.size_bytes || sha256_hex(&bytes) != artifact.sha256 {
            return Err(RuntimeError::InvalidCommand(
                "contained analysis artifact failed content verification".to_owned(),
            ));
        }
        let operation_json = serde_json::to_string(&operation_wire(&operation)?)
            .map_err(RuntimeError::agent)?;
        let program = python_program();
        let args = vec![
            "-I".to_owned(),
            "-B".to_owned(),
            "-c".to_owned(),
            PYTHON_REDUCER.to_owned(),
            artifact_path.to_string_lossy().into_owned(),
            operation_json,
        ];
        let started = Instant::now();
        let output = medusa_agent::run_contained_analysis_command(
            &root,
            program,
            &args,
            &self.cancel,
        )
        .map_err(RuntimeError::agent)?;
        if !output.status.success() {
            return Err(RuntimeError::InvalidCommand(format!(
                "contained analysis backend failed: {}",
                bounded_text(&output.stderr)
            )));
        }
        if output.stdout.len() > MAX_CONTAINED_OUTPUT_BYTES {
            return Err(RuntimeError::InvalidCommand(
                "contained analysis output exceeded the configured byte limit".to_owned(),
            ));
        }
        let wire: WireResult = serde_json::from_slice(&output.stdout).map_err(RuntimeError::agent)?;
        let result = AnalysisResult {
            operation,
            value: wire.value.into_value()?,
            provenance: vec![AnalysisProvenance {
                artifact_sha256: artifact.sha256.clone(),
                source_path: artifact.source.repository_relative_path.clone(),
                source_sha256: artifact.source.source_sha256.clone(),
                byte_range: artifact.source.byte_range,
            }],
            truncated: wire.truncated,
        };
        Ok(ContainedAnalysisResult {
            metrics: AnalysisExecutionMetrics {
                input_bytes: artifact.size_bytes,
                output_bytes: output.stdout.len(),
                elapsed_millis: started.elapsed().as_millis(),
            },
            result,
            backend: "contained_fixed_python_reducer".to_owned(),
        })
    }
}

#[derive(Deserialize)]
struct WireResult {
    value: WireValue,
    truncated: bool,
}

#[derive(Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
enum WireValue {
    Bool(bool),
    Integer(i64),
    StringList(Vec<String>),
}

impl WireValue {
    fn into_value(self) -> Result<AnalysisValue, RuntimeError> {
        match self {
            Self::Bool(value) => Ok(AnalysisValue::Bool(value)),
            Self::Integer(value) => Ok(AnalysisValue::Integer(value)),
            Self::StringList(values) => {
                if values.len() > MAX_CONTAINED_LINES
                    || values.iter().map(String::len).sum::<usize>() > MAX_CONTAINED_OUTPUT_BYTES
                {
                    return Err(RuntimeError::InvalidCommand(
                        "contained analysis returned an out-of-bounds collection".to_owned(),
                    ));
                }
                Ok(AnalysisValue::StringList(values))
            }
        }
    }
}

fn operation_wire(operation: &AnalysisOperation) -> Result<Value, RuntimeError> {
    match operation {
        AnalysisOperation::ByteCount => Ok(serde_json::json!({"kind": "byte_count"})),
        AnalysisOperation::LineCount => Ok(serde_json::json!({"kind": "line_count"})),
        AnalysisOperation::Utf8Contains { needle } => {
            Ok(serde_json::json!({"kind": "utf8_contains", "needle": needle}))
        }
        AnalysisOperation::MatchingLines { needle, limit } => Ok(serde_json::json!({
            "kind": "matching_lines",
            "needle": needle,
            "limit": (*limit).min(MAX_CONTAINED_LINES),
        })),
        AnalysisOperation::HeadLines { limit } => Ok(serde_json::json!({
            "kind": "head_lines",
            "limit": (*limit).min(MAX_CONTAINED_LINES),
        })),
    }
}

fn analysis_root(repo: &Path, session_id: &str) -> PathBuf {
    repo.join(".medusa")
        .join("analysis-workspace-v1")
        .join(sha256_hex(session_id.as_bytes()))
}

fn python_program() -> &'static str {
    #[cfg(windows)]
    {
        "python.exe"
    }
    #[cfg(not(windows))]
    {
        "python3"
    }
}

fn bounded_text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(&bytes[..bytes.len().min(2048)]).into_owned()
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Config;
    use tempfile::TempDir;

    #[test]
    fn capabilities_fail_closed_by_default() {
        let temp = TempDir::new().expect("tempdir");
        let controller = RuntimeController::start_with_config(
            temp.path().to_path_buf(),
            Config::default(),
        );
        let capabilities = controller.analysis_workspace_capabilities();
        assert!(!capabilities.arbitrary_user_code);
        assert!(!capabilities.ambient_network);
        assert!(!capabilities.ambient_credentials);
        assert!(!capabilities.primary_repository_write);
        assert!(!capabilities.direct_provider_client);
        assert_eq!(capabilities.process_limit, 1);
    }

    #[test]
    fn operation_wire_is_bounded_and_never_contains_code() {
        let wire = operation_wire(&AnalysisOperation::MatchingLines {
            needle: "needle".to_owned(),
            limit: usize::MAX,
        })
        .expect("wire");
        assert_eq!(wire["kind"], "matching_lines");
        assert_eq!(wire["limit"], MAX_CONTAINED_LINES);
        assert!(wire.get("code").is_none());
    }
}
