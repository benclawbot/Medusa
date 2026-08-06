use std::{fs, path::{Path, PathBuf}};

use medusa_core::MedusaResult;
use medusa_intelligence::{
    CodeIndex, PatchTransaction, TextEdit, TypeScriptSemanticOperation,
    TypeScriptSemanticRequest, execute_typescript_semantic, format_changed,
    language_capability_profiles, select_tests,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::{
    policy::safe_path,
    tools::{input_string, input_usize},
};

pub(crate) fn semantic_capabilities() -> MedusaResult<String> {
    Ok(serde_json::to_string_pretty(&json!({
        "profiles": language_capability_profiles(),
    }))?)
}

pub(crate) fn code_index(repo: &Path, input: &Value) -> MedusaResult<String> {
    let index = CodeIndex::build(repo)?;
    if let Some(name) = input.get("name").and_then(Value::as_str) {
        Ok(serde_json::to_string_pretty(&json!({
            "definitions": index.definitions(name),
            "references": index.references(name),
            "parse_errors": index.parse_errors,
        }))?)
    } else {
        Ok(serde_json::to_string_pretty(&index)?)
    }
}

pub(crate) fn patch_apply(repo: &Path, input: &Value) -> MedusaResult<String> {
    let edits = input
        .get("edits")
        .and_then(Value::as_array)
        .ok_or_else(|| crate::tools::invalid_tool("edits must be an array"))?;
    let mut transaction = PatchTransaction::new();
    for edit in edits {
        let relative = input_string(edit, "path")?;
        let _ = safe_path(repo, relative)?;
        transaction.add_edit(TextEdit {
            path: PathBuf::from(relative),
            start_byte: input_usize(edit, "start_byte")?,
            end_byte: input_usize(edit, "end_byte")?,
            expected: input_string(edit, "expected")?.to_owned(),
            replacement: input_string(edit, "replacement")?.to_owned(),
        })?;
    }
    let receipt = transaction.commit(repo)?;
    let formatting = format_changed(repo, &receipt.changed_paths)?;
    let impact = select_tests(&receipt.changed_paths);
    Ok(serde_json::to_string_pretty(&json!({
        "receipt": receipt,
        "formatting": formatting,
        "test_impact": impact,
    }))?)
}

pub(crate) fn symbol_rename(repo: &Path, input: &Value) -> MedusaResult<String> {
    let old_name = input_string(input, "old_name")?;
    let new_name = input_string(input, "new_name")?;
    let index = CodeIndex::build(repo)?;
    let mut transaction = PatchTransaction::new();
    let references = transaction.rename_symbol(&index, old_name, new_name)?;
    for reference in index.references(old_name) {
        let _ = safe_path(repo, reference.path.to_string_lossy().as_ref())?;
    }
    let receipt = transaction.commit(repo)?;
    let formatting = format_changed(repo, &receipt.changed_paths)?;
    let impact = select_tests(&receipt.changed_paths);
    Ok(serde_json::to_string_pretty(&json!({
        "renamed_references": references,
        "receipt": receipt,
        "formatting": formatting,
        "test_impact": impact,
    }))?)
}


pub(crate) fn typescript_semantic(repo: &Path, input: &Value) -> MedusaResult<String> {
    let operation = match input_string(input, "operation")? {
        "definition" => TypeScriptSemanticOperation::Definition,
        "references" => TypeScriptSemanticOperation::References,
        "diagnostics" => TypeScriptSemanticOperation::Diagnostics,
        "workspace_symbols" => TypeScriptSemanticOperation::WorkspaceSymbols,
        other => return Err(crate::tools::invalid_tool(format!("unsupported TypeScript semantic operation: {other}"))),
    };
    let path = input.get("path").and_then(Value::as_str).map(PathBuf::from);
    if matches!(operation, TypeScriptSemanticOperation::Definition | TypeScriptSemanticOperation::References)
        && path.is_none()
    {
        return Err(crate::tools::invalid_tool("path is required for position-based TypeScript operations"));
    }
    if let Some(path) = &path { let _ = safe_path(repo, path.to_string_lossy().as_ref())?; }
    let response = execute_typescript_semantic(repo, &TypeScriptSemanticRequest {
        operation,
        path,
        line: optional_u32(input, "line")?,
        character: optional_u32(input, "character")?,
        query: input.get("query").and_then(Value::as_str).map(str::to_owned),
        new_name: None,
        expected_workspace_fingerprint: input.get("expected_workspace_fingerprint").and_then(Value::as_str).map(str::to_owned),
    }).map_err(|error| crate::tools::invalid_tool(error.to_string()))?;
    Ok(serde_json::to_string_pretty(&response)?)
}

pub(crate) fn typescript_rename(repo: &Path, input: &Value) -> MedusaResult<String> {
    let relative = input_string(input, "path")?;
    let _ = safe_path(repo, relative)?;
    let response = execute_typescript_semantic(repo, &TypeScriptSemanticRequest {
        operation: TypeScriptSemanticOperation::Rename,
        path: Some(PathBuf::from(relative)),
        line: Some(required_u32(input, "line")?),
        character: Some(required_u32(input, "character")?),
        query: None,
        new_name: Some(input_string(input, "new_name")?.to_owned()),
        expected_workspace_fingerprint: input.get("expected_workspace_fingerprint").and_then(Value::as_str).map(str::to_owned),
    }).map_err(|error| crate::tools::invalid_tool(error.to_string()))?;
    let edits = response.pointer("/result/edits").and_then(Value::as_array)
        .ok_or_else(|| crate::tools::invalid_tool("TypeScript rename response is missing edits"))?;
    let mut transaction = PatchTransaction::new();
    for edit in edits {
        let path = input_string(edit, "path")?;
        let absolute = safe_path(repo, path)?;
        let content = fs::read(&absolute)?;
        let actual_hash = hex::encode(Sha256::digest(&content));
        let expected_hash = input_string(edit, "source_hash")?;
        if actual_hash != expected_hash {
            return Err(crate::tools::invalid_tool(format!("stale TypeScript rename source: {path}")));
        }
        transaction.add_edit(TextEdit {
            path: PathBuf::from(path),
            start_byte: input_usize(edit, "start_byte")?,
            end_byte: input_usize(edit, "end_byte")?,
            expected: input_string(edit, "expected")?.to_owned(),
            replacement: input_string(edit, "replacement")?.to_owned(),
        })?;
    }
    let receipt = transaction.commit(repo)?;
    let formatting = format_changed(repo, &receipt.changed_paths)?;
    let impact = select_tests(&receipt.changed_paths);
    Ok(serde_json::to_string_pretty(&json!({
        "semantic_evidence": response.get("evidence"),
        "renamed_references": edits.len(),
        "receipt": receipt,
        "formatting": formatting,
        "test_impact": impact,
    }))?)
}

fn optional_u32(input: &Value, key: &str) -> MedusaResult<Option<u32>> {
    match input.get(key) {
        None => Ok(None),
        Some(value) => value.as_u64().and_then(|value| u32::try_from(value).ok()).map(Some)
            .ok_or_else(|| crate::tools::invalid_tool(format!("{key} must be a non-negative 32-bit integer"))),
    }
}

fn required_u32(input: &Value, key: &str) -> MedusaResult<u32> {
    optional_u32(input, key)?.ok_or_else(|| crate::tools::invalid_tool(format!("{key} is required")))
}
