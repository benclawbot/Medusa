use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write,
    path::Path,
    process::{Command, Stdio},
    time::Duration,
};

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    redaction::{redact, redact_value},
    support::{file_digest, internal, invalid, wait_with_timeout},
};

/// Pinned MCP server registry entry.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct McpRegistryEntry {
    pub id: String,
    pub source: String,
    pub digest: String,
    pub transport: String,
    pub trust: String,
    pub capabilities: BTreeSet<String>,
    pub environment_allowlist: BTreeSet<String>,
    pub network_allowlist: BTreeSet<String>,
    pub sandbox: String,
}

/// Minimal MCP request envelope used by the isolated stdio transport.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct McpRequest {
    pub jsonrpc: String,
    pub id: u64,
    pub method: String,
    pub params: Value,
}

/// Audited MCP response. Returned text is always untrusted data.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct McpResponse {
    pub origin: String,
    pub untrusted: bool,
    pub payload: Value,
}

/// Invokes one pinned stdio MCP process in an environment-cleared sandbox directory.
pub fn call_mcp_stdio(
    entry: &McpRegistryEntry,
    executable: &Path,
    args: &[String],
    sandbox_directory: &Path,
    request: &McpRequest,
    source_environment: &BTreeMap<String, String>,
    timeout: Duration,
) -> MedusaResult<McpResponse> {
    validate_mcp_entry(entry, executable)?;
    fs::create_dir_all(sandbox_directory)?;
    let mut command = Command::new(executable);
    command
        .args(args)
        .current_dir(sandbox_directory)
        .env_clear()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for key in &entry.environment_allowlist {
        if let Some(value) = source_environment.get(key) {
            command.env(key, value);
        }
    }
    let mut child = command.spawn()?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| internal("MCP stdin unavailable"))?;
    serde_json::to_writer(&mut stdin, request)?;
    stdin.write_all(b"\n")?;
    drop(stdin);
    let output = wait_with_timeout(child, timeout)?;
    if !output.status.success() {
        return Err(MedusaError::new(
            ErrorCode::ToolExecutionFailed,
            ErrorCategory::Execution,
            format!(
                "MCP {} failed: {}",
                entry.id,
                redact(&String::from_utf8_lossy(&output.stderr))
            ),
        ));
    }
    let mut payload: Value = serde_json::from_slice(&output.stdout)?;
    redact_value(&mut payload);
    validate_mcp_output(&payload)?;
    Ok(McpResponse {
        origin: entry.id.clone(),
        untrusted: true,
        payload,
    })
}

fn validate_mcp_entry(entry: &McpRegistryEntry, executable: &Path) -> MedusaResult<()> {
    if entry.transport != "stdio"
        || entry.source.trim().is_empty()
        || entry.digest.trim().is_empty()
    {
        return Err(invalid("MCP entry must be pinned and use stdio"));
    }
    let actual = file_digest(executable)?;
    if actual != entry.digest {
        return Err(MedusaError::new(
            ErrorCode::ChecksumMismatch,
            ErrorCategory::Validation,
            format!("MCP executable digest mismatch for {}", entry.id),
        ));
    }
    Ok(())
}

pub(crate) fn validate_mcp_output(payload: &Value) -> MedusaResult<()> {
    let serialized = serde_json::to_string(payload)?.to_ascii_lowercase();
    for forbidden in [
        "ignore previous instructions",
        "redefine system policy",
        "grant me additional tools",
    ] {
        if serialized.contains(forbidden) {
            return Err(MedusaError::new(
                ErrorCode::PolicyDenied,
                ErrorCategory::Policy,
                format!("MCP tool-poisoning content rejected: {forbidden}"),
            ));
        }
    }
    Ok(())
}
