use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};

use medusa_core::MedusaResult;
use medusa_intelligence::{
    CodeIndex, LspCapabilityResult, LspClient, PatchTransaction, TextEdit,
    bind_guarded_rename_snapshot, discover_typescript_workspace, find_references, format_changed,
    go_to_definition, language_capability_profiles, lsp_rename, prepare_guarded_rename_transaction,
    prepare_rename, select_tests, validate_guarded_rename, workspace_symbols,
};
use serde_json::{Value, json};

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

pub(crate) fn typescript_semantic(repo: &Path, input: &Value) -> MedusaResult<String> {
    let operation = input_string(input, "operation")?;
    let target = input.get("path").and_then(Value::as_str).unwrap_or(".");
    let target_path = safe_path(repo, target)?;
    let workspace = discover_typescript_workspace(repo, &target_path)
        .map_err(|error| crate::tools::invalid_tool(error.to_string()))?;
    let mut client = LspClient::new(workspace.server_config());
    client
        .start()
        .map_err(|error| crate::tools::invalid_tool(error.to_string()))?;

    let result = match operation {
        "workspace_symbols" => workspace_symbols(
            &mut client,
            &workspace.workspace_root,
            input.get("query").and_then(Value::as_str).unwrap_or(""),
        )
        .map(|result| json!(result)),
        "definition" | "references" | "diagnostics" => {
            if !target_path.is_file() {
                return Err(crate::tools::invalid_tool(
                    "path must identify a source file",
                ));
            }
            let text = fs::read_to_string(&target_path)?;
            let uri = file_uri(&target_path);
            client
                .notify(
                    "textDocument/didOpen",
                    json!({
                        "textDocument": {
                            "uri": uri,
                            "languageId": language_id(&target_path),
                            "version": 1,
                            "text": text,
                        }
                    }),
                )
                .map_err(|error| crate::tools::invalid_tool(error.to_string()))?;
            let line = input_usize(input, "line").unwrap_or(0) as u32;
            let character = input_usize(input, "character").unwrap_or(0) as u32;
            match operation {
                "definition" => go_to_definition(
                    &mut client,
                    &workspace.workspace_root,
                    &target_path,
                    line,
                    character,
                )
                .map(|result| json!(result)),
                "references" => find_references(
                    &mut client,
                    &workspace.workspace_root,
                    &target_path,
                    line,
                    character,
                    true,
                )
                .map(|result| json!(result)),
                "diagnostics" => {
                    let _ = workspace_symbols(&mut client, &workspace.workspace_root, "");
                    let diagnostics = client
                        .drain_notifications()
                        .into_iter()
                        .filter(|message| {
                            message.get("method").and_then(Value::as_str)
                                == Some("textDocument/publishDiagnostics")
                        })
                        .collect::<Vec<_>>();
                    Ok(json!({"notifications": diagnostics}))
                }
                _ => unreachable!(),
            }
        }
        _ => {
            return Err(crate::tools::invalid_tool(
                "unsupported TypeScript semantic operation",
            ));
        }
    }
    .map_err(|error| crate::tools::invalid_tool(error.to_string()))?;

    Ok(serde_json::to_string_pretty(&json!({
        "workspace": workspace,
        "operation": operation,
        "result": result,
    }))?)
}

fn file_uri(path: &Path) -> String {
    let normalized = path.to_string_lossy().replace('\\', "/");
    if normalized.starts_with('/') {
        format!("file://{normalized}")
    } else {
        format!("file:///{normalized}")
    }
}

fn language_id(path: &Path) -> &'static str {
    match path.extension().and_then(|value| value.to_str()) {
        Some("js" | "jsx" | "mjs" | "cjs") => "javascript",
        Some("tsx") => "typescriptreact",
        _ => "typescript",
    }
}

fn is_typescript_path(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|value| value.to_str()),
        Some("ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs")
    )
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
    let indexed_references = index.references(old_name);
    let has_typescript_reference = indexed_references
        .iter()
        .any(|reference| is_typescript_path(&reference.path));
    let has_non_typescript_reference = indexed_references
        .iter()
        .any(|reference| !is_typescript_path(&reference.path));

    if has_non_typescript_reference || !has_typescript_reference {
        return rust_symbol_rename(repo, &index, old_name, new_name);
    }

    typescript_symbol_rename(repo, old_name, new_name)
}

fn rust_symbol_rename(
    repo: &Path,
    index: &CodeIndex,
    old_name: &str,
    new_name: &str,
) -> MedusaResult<String> {
    let mut transaction = PatchTransaction::new();
    let references = transaction.rename_symbol(index, old_name, new_name)?;
    for reference in index.references(old_name) {
        let _ = safe_path(repo, reference.path.to_string_lossy().as_ref())?;
    }
    let receipt = transaction.commit(repo)?;
    let formatting = format_changed(repo, &receipt.changed_paths)?;
    let impact = select_tests(&receipt.changed_paths);
    Ok(serde_json::to_string_pretty(&json!({
        "language": "rust",
        "renamed_references": references,
        "receipt": receipt,
        "formatting": formatting,
        "test_impact": impact,
    }))?)
}

fn typescript_symbol_rename(repo: &Path, old_name: &str, new_name: &str) -> MedusaResult<String> {
    let workspace = discover_typescript_workspace(repo, repo)
        .map_err(|error| crate::tools::invalid_tool(error.to_string()))?;
    let mut client = LspClient::new(workspace.server_config());
    client
        .start()
        .map_err(|error| crate::tools::invalid_tool(error.to_string()))?;

    let symbols = workspace_symbols(&mut client, repo, old_name)
        .map_err(|error| crate::tools::invalid_tool(error.to_string()))?;
    let mut candidates = symbols
        .locations
        .into_iter()
        .filter(|location| {
            location.name.as_deref() == Some(old_name) && is_typescript_path(&location.path)
        })
        .collect::<Vec<_>>();
    candidates.sort_by(|left, right| {
        left.path
            .cmp(&right.path)
            .then(left.range.cmp(&right.range))
    });
    candidates.dedup_by(|left, right| left.path == right.path && left.range == right.range);

    let location = match candidates.as_slice() {
        [location] => location.clone(),
        [] => {
            return Err(crate::tools::invalid_tool(format!(
                "TypeScript rename refused because `{old_name}` has no exact workspace symbol"
            )));
        }
        _ => {
            return Err(crate::tools::invalid_tool(format!(
                "TypeScript rename refused because `{old_name}` is ambiguous across {} workspace symbols",
                candidates.len()
            )));
        }
    };

    let target_path = safe_path(repo, location.path.to_string_lossy().as_ref())?;
    if !target_path.is_file() {
        return Err(crate::tools::invalid_tool(
            "TypeScript rename target must identify a source file",
        ));
    }
    let text = fs::read_to_string(&target_path)?;
    client
        .notify(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": file_uri(&target_path),
                    "languageId": language_id(&target_path),
                    "version": 1,
                    "text": text,
                }
            }),
        )
        .map_err(|error| crate::tools::invalid_tool(error.to_string()))?;

    let position = location.range.start.clone();
    match prepare_rename(&mut client, &target_path, position.clone())
        .map_err(|error| crate::tools::invalid_tool(error.to_string()))?
    {
        LspCapabilityResult::Supported(_) => {}
        LspCapabilityResult::Unsupported => {
            return Err(crate::tools::invalid_tool(
                "TypeScript rename refused because the language server does not support prepareRename",
            ));
        }
        LspCapabilityResult::Disabled { reason } => {
            return Err(crate::tools::invalid_tool(format!(
                "TypeScript rename refused because prepareRename is disabled: {reason}"
            )));
        }
    }

    let edit = match lsp_rename(&mut client, repo, &target_path, position.clone(), new_name)
        .map_err(|error| crate::tools::invalid_tool(error.to_string()))?
    {
        LspCapabilityResult::Supported(edit) => edit,
        LspCapabilityResult::Unsupported => {
            return Err(crate::tools::invalid_tool(
                "TypeScript rename refused because the language server returned no workspace edit",
            ));
        }
        LspCapabilityResult::Disabled { reason } => {
            return Err(crate::tools::invalid_tool(format!(
                "TypeScript rename refused because rename is disabled: {reason}"
            )));
        }
    };

    let references = find_references(
        &mut client,
        repo,
        &target_path,
        position.line,
        position.character,
        true,
    )
    .map_err(|error| crate::tools::invalid_tool(error.to_string()))?;
    if references.unsupported {
        return Err(crate::tools::invalid_tool(
            "TypeScript rename refused because independent reference discovery is unsupported",
        ));
    }

    let static_paths = references
        .locations
        .iter()
        .map(|reference| reference.path.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    if static_paths.is_empty() {
        return Err(crate::tools::invalid_tool(
            "TypeScript rename refused because independent reference discovery returned no paths",
        ));
    }
    for path in &static_paths {
        let _ = safe_path(repo, path.to_string_lossy().as_ref())?;
    }

    let plan = validate_guarded_rename(edit, &static_paths, &static_paths)
        .map_err(crate::tools::invalid_tool)?;
    let bound = bind_guarded_rename_snapshot(repo, plan).map_err(crate::tools::invalid_tool)?;
    let transaction =
        prepare_guarded_rename_transaction(repo, &bound).map_err(crate::tools::invalid_tool)?;
    let receipt = transaction.commit(repo)?;
    let formatting = format_changed(repo, &receipt.changed_paths)?;
    let impact = select_tests(&receipt.changed_paths);

    Ok(serde_json::to_string_pretty(&json!({
        "language": "typescript_javascript",
        "renamed_references": references.locations.len(),
        "workspace": workspace,
        "receipt": receipt,
        "formatting": formatting,
        "test_impact": impact,
    }))?)
}
