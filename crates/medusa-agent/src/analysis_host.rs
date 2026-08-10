//! Dependency-neutral host bridge for the subordinate analysis workspace.
//!
//! `medusa-agent` owns only the model-facing contract. The runtime supplies the authority and
//! implementation, so analysis code cannot construct providers, schedulers, or mutation paths.

use std::sync::atomic::AtomicBool;

use medusa_core::MedusaResult;
use medusa_provider::ToolDefinition;
use serde_json::{Value, json};

pub const ANALYSIS_WORKSPACE_TOOL: &str = "analysis_workspace";

/// Runtime-owned authority invoked by the agent tool loop.
pub trait AnalysisWorkspaceHost: Send + Sync {
    fn execute(
        &self,
        session_id: &str,
        input: &Value,
        cancellation: &AtomicBool,
    ) -> MedusaResult<String>;
}

#[must_use]
pub fn tool_definition() -> ToolDefinition {
    ToolDefinition {
        name: ANALYSIS_WORKSPACE_TOOL.to_owned(),
        description: "Use Medusa's contained persistent analysis workspace for brokered repository data, bounded reductions, snapshots, and scheduler-governed read-only delegation. This tool cannot execute arbitrary user code or mutate the repository directly.".to_owned(),
        input_schema: json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["action"],
            "properties": {
                "action": {
                    "type": "string",
                    "enum": [
                        "import_reduce",
                        "set_value",
                        "list_values",
                        "snapshot",
                        "restore",
                        "delegate_read_only",
                        "list_children",
                        "follow_up_child",
                        "await_child"
                    ]
                },
                "path": {"type": "string"},
                "byte_start": {"type": "integer", "minimum": 0},
                "byte_end": {"type": "integer", "minimum": 0},
                "operation": {
                    "type": "string",
                    "enum": ["byte_count", "line_count", "utf8_contains", "matching_lines", "head_lines"]
                },
                "needle": {"type": "string", "maxLength": 4096},
                "limit": {"type": "integer", "minimum": 0, "maximum": 128},
                "name": {"type": "string", "maxLength": 128},
                "value": {},
                "objective": {"type": "string", "maxLength": 8192},
                "worker_id": {"type": "string", "maxLength": 256},
                "message": {"type": "string", "maxLength": 4096}
            }
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contract_exposes_no_code_or_mutation_field() {
        let definition = tool_definition();
        let properties = definition.input_schema["properties"]
            .as_object()
            .expect("properties");
        assert!(!properties.contains_key("code"));
        assert!(!properties.contains_key("command"));
        assert!(!properties.contains_key("write_path"));
        assert!(!properties.contains_key("provider"));
    }
}
