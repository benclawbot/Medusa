use std::path::{Path, PathBuf};

use medusa_core::MedusaResult;
use medusa_intelligence::{CodeIndex, PatchTransaction, TextEdit, format_changed, select_tests};
use serde_json::{Value, json};

use crate::{
    policy::safe_path,
    tools::{input_string, input_usize},
};

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
