//! Versioned model-facing Code Mode presentation over effective admitted tools.
//!
//! This module generates a typed SDK description only. Execution remains in the existing
//! certified tool pipeline; a caller must pass every nested invocation back through that
//! authority and apply the bounded limits below.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use medusa_provider::ToolDefinition;

use crate::tool_result::{CanonicalToolOutcome, CanonicalToolResultV1};

pub const CODE_MODE_SCHEMA_VERSION: u16 = 1;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolPresentationMode {
    Native,
    Code,
    Both,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CodeModeLimits {
    pub max_program_chars: usize,
    pub max_cpu_millis: u64,
    pub max_memory_bytes: u64,
    pub max_output_chars: usize,
    pub max_nested_calls: usize,
}

impl Default for CodeModeLimits {
    fn default() -> Self {
        Self {
            max_program_chars: 64 * 1024,
            max_cpu_millis: 30_000,
            max_memory_bytes: 256 * 1024 * 1024,
            max_output_chars: 256 * 1024,
            max_nested_calls: 64,
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct CodeModeRequest {
    pub schema_version: u16,
    pub program: String,
    pub requested_limits: CodeModeLimits,
}

impl CodeModeRequest {
    pub fn validate(&self, hard_limits: CodeModeLimits) -> MedusaResult<()> {
        if self.schema_version != CODE_MODE_SCHEMA_VERSION {
            return Err(code_mode_error("unsupported Code Mode request schema"));
        }
        if self.program.trim().is_empty() {
            return Err(code_mode_error("Code Mode program must not be empty"));
        }
        if self.requested_limits.max_program_chars > hard_limits.max_program_chars
            || self.requested_limits.max_cpu_millis > hard_limits.max_cpu_millis
            || self.requested_limits.max_memory_bytes > hard_limits.max_memory_bytes
            || self.requested_limits.max_output_chars > hard_limits.max_output_chars
            || self.requested_limits.max_nested_calls > hard_limits.max_nested_calls
        {
            return Err(code_mode_error(
                "Code Mode request exceeds the certified hard limits",
            ));
        }
        if self.program.chars().count() > self.requested_limits.max_program_chars {
            return Err(code_mode_error(
                "Code Mode program exceeds its requested limit",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CodeModeTool {
    pub name: String,
    pub identifier: String,
    pub description: String,
    pub input_schema: Value,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CodeModeSdkV1 {
    pub schema_version: u16,
    pub presentation_mode: ToolPresentationMode,
    pub admitted_tool_fingerprint: String,
    pub tools: Vec<CodeModeTool>,
    pub typescript_source: String,
}

/// Bounded, serializable Code Mode IR. A provider adapter may compile a generated SDK program
/// into this form, but the IR itself contains only admitted tool calls and cannot express journal,
/// verifier, integration, delegation, or policy operations.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CodeModeProgramV1 {
    pub schema_version: u16,
    pub calls: Vec<CodeModeCallV1>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CodeModeCallV1 {
    pub tool: String,
    pub input: Value,
}

impl CodeModeProgramV1 {
    pub fn validate(&self, sdk: &CodeModeSdkV1, hard_limits: CodeModeLimits) -> MedusaResult<()> {
        if self.schema_version != CODE_MODE_SCHEMA_VERSION {
            return Err(code_mode_error("unsupported Code Mode program schema"));
        }
        if self.calls.is_empty() {
            return Err(code_mode_error("Code Mode program must contain a call"));
        }
        if self.calls.len() > hard_limits.max_nested_calls {
            return Err(code_mode_error(
                "Code Mode program exceeds the certified nested-call limit",
            ));
        }
        for call in &self.calls {
            if sdk.tools.iter().all(|tool| tool.name != call.tool) {
                return Err(code_mode_error(format!(
                    "Code Mode tool is not in the effective admitted SDK: {}",
                    call.tool
                )));
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct CodeModeChildResultV1 {
    pub ordinal: u32,
    pub tool: String,
    pub receipt: Value,
    pub canonical: CanonicalToolResultV1,
    /// Deterministic parent aggregate identity for cross-tool attribution.
    #[serde(default)]
    pub parent_execution_fingerprint: Option<String>,
}

impl CodeModeChildResultV1 {
    #[must_use]
    pub fn from_certified(
        ordinal: u32,
        tool: &str,
        receipt: Value,
        canonical: CanonicalToolResultV1,
    ) -> Self {
        Self {
            ordinal,
            tool: tool.to_owned(),
            receipt,
            canonical,
            parent_execution_fingerprint: None,
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct CodeModeExecutionV1 {
    pub schema_version: u16,
    pub outcome: CanonicalToolOutcome,
    pub children: Vec<CodeModeChildResultV1>,
    pub model_projection: Value,
    pub execution_fingerprint: String,
}

impl CodeModeExecutionV1 {
    pub fn from_children(mut children: Vec<CodeModeChildResultV1>) -> MedusaResult<Self> {
        let outcome = if children
            .iter()
            .all(|child| child.canonical.outcome == CanonicalToolOutcome::Success)
        {
            CanonicalToolOutcome::Success
        } else if children
            .iter()
            .any(|child| child.canonical.outcome == CanonicalToolOutcome::Denied)
        {
            CanonicalToolOutcome::Denied
        } else if children
            .iter()
            .any(|child| child.canonical.outcome == CanonicalToolOutcome::Cancelled)
        {
            CanonicalToolOutcome::Cancelled
        } else {
            CanonicalToolOutcome::Failed
        };
        let material = serde_json::to_vec(
            &children
                .iter()
                .map(|child| {
                    (
                        child.ordinal,
                        child.tool.as_str(),
                        &child.canonical.outcome,
                        child.canonical.source_fingerprint.as_str(),
                    )
                })
                .collect::<Vec<_>>(),
        )?;
        let execution_fingerprint = digest(&material);
        for child in &mut children {
            child.parent_execution_fingerprint = Some(execution_fingerprint.clone());
        }
        let model_projection = json!({
            "schema_version": CODE_MODE_SCHEMA_VERSION,
            "outcome": outcome,
            "children": children.iter().map(|child| json!({
                "ordinal": child.ordinal,
                "tool": child.tool,
                "outcome": child.canonical.outcome,
                "source_fingerprint": child.canonical.source_fingerprint,
                "parent_execution_fingerprint": child.parent_execution_fingerprint,
            })).collect::<Vec<_>>(),
        });
        Ok(Self {
            schema_version: CODE_MODE_SCHEMA_VERSION,
            outcome,
            children,
            model_projection,
            execution_fingerprint: digest(&material),
        })
    }
}

pub fn generate_sdk(definitions: &[ToolDefinition]) -> MedusaResult<CodeModeSdkV1> {
    let mut definitions = definitions.to_vec();
    definitions.sort_by(|left, right| left.name.cmp(&right.name));
    let mut tools = Vec::with_capacity(definitions.len());
    let mut source = String::from(
        "// Generated from the effective admitted tool registry.\n\
         // Each call must re-enter Medusa's certified pipeline.\n\n\
         export interface MedusaCodeModeApi {\n",
    );
    for definition in definitions {
        let identifier = identifier(&definition.name);
        let input_type = typescript_type(&definition.input_schema);
        source.push_str(&format!(
            "  /** {} */\n  {}(input: {}): Promise<unknown>;\n",
            definition.description.replace("*/", "* /"),
            identifier,
            input_type
        ));
        tools.push(CodeModeTool {
            name: definition.name,
            identifier,
            description: definition.description,
            input_schema: definition.input_schema,
        });
    }
    source.push_str("}\n");
    let fingerprint_material = serde_json::to_vec(&tools)
        .map_err(|error| code_mode_error(format!("cannot fingerprint admitted tools: {error}")))?;
    Ok(CodeModeSdkV1 {
        schema_version: CODE_MODE_SCHEMA_VERSION,
        presentation_mode: ToolPresentationMode::Code,
        admitted_tool_fingerprint: digest(&fingerprint_material),
        tools,
        typescript_source: source,
    })
}

fn identifier(name: &str) -> String {
    let mut value = name
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '_' {
                character
            } else {
                '_'
            }
        })
        .collect::<String>();
    if value.is_empty() {
        value.push_str("tool");
    }
    if value
        .chars()
        .next()
        .is_some_and(|character| character.is_ascii_digit())
    {
        value.insert(0, '_');
    }
    value
}

fn typescript_type(schema: &Value) -> String {
    match schema.get("type").and_then(Value::as_str) {
        Some("string") => "string".to_owned(),
        Some("integer") | Some("number") => "number".to_owned(),
        Some("boolean") => "boolean".to_owned(),
        Some("array") => format!(
            "Array<{}>",
            schema
                .get("items")
                .map_or_else(|| "unknown".to_owned(), typescript_type)
        ),
        Some("object") => {
            let Some(properties) = schema.get("properties").and_then(Value::as_object) else {
                return "Record<string, unknown>".to_owned();
            };
            let required = schema
                .get("required")
                .and_then(Value::as_array)
                .map(|values| values.iter().filter_map(Value::as_str).collect::<Vec<_>>())
                .unwrap_or_default();
            let mut fields = properties
                .iter()
                .map(|(name, value)| {
                    let optional = (!required.contains(&name.as_str())).then_some("?");
                    format!(
                        "{}{}: {}",
                        name.replace(['-', ' '], "_"),
                        optional.unwrap_or_default(),
                        typescript_type(value)
                    )
                })
                .collect::<Vec<_>>();
            fields.sort();
            format!("{{ {} }}", fields.join("; "))
        }
        _ => "unknown".to_owned(),
    }
}

fn digest(value: &[u8]) -> String {
    Sha256::digest(value)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn code_mode_error(message: impl Into<String>) -> MedusaError {
    MedusaError::new(
        ErrorCode::InvalidConfiguration,
        ErrorCategory::Validation,
        message,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn sdk_is_deterministic_and_generated_from_admitted_definitions() {
        let definitions = vec![
            ToolDefinition {
                name: "fs-write".to_owned(),
                description: "write".to_owned(),
                input_schema: json!({
                    "type": "object",
                    "properties": {"path": {"type": "string"}},
                    "required": ["path"]
                }),
            },
            ToolDefinition {
                name: "fs_read".to_owned(),
                description: "read".to_owned(),
                input_schema: json!({"type": "object"}),
            },
        ];
        let sdk = generate_sdk(&definitions).expect("sdk");
        assert_eq!(sdk.tools[0].name, "fs-write");
        assert!(sdk.typescript_source.contains("fs_write(input:"));
        assert_ne!(sdk.admitted_tool_fingerprint, "");
        assert_eq!(sdk, generate_sdk(&definitions).expect("repeat"));
    }

    #[test]
    fn code_mode_cannot_expand_hard_limits() {
        let request = CodeModeRequest {
            schema_version: CODE_MODE_SCHEMA_VERSION,
            program: "return 1".to_owned(),
            requested_limits: CodeModeLimits {
                max_nested_calls: 2,
                ..CodeModeLimits::default()
            },
        };
        request
            .validate(CodeModeLimits::default())
            .expect("bounded request");
        let mut excessive = request;
        excessive.requested_limits.max_nested_calls = 65;
        assert!(excessive.validate(CodeModeLimits::default()).is_err());
    }
}
