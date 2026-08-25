use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use medusa_core::storage::file_uri;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{LspClient, LspError, LspPosition, LspRange};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LspCapabilityResult<T> {
    Supported(T),
    Unsupported,
    Disabled { reason: String },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LspChangeAnnotation {
    pub label: String,
    pub needs_confirmation: bool,
    pub description: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LspAnnotatedTextEdit {
    pub path: PathBuf,
    pub range: LspRange,
    pub new_text: String,
    pub annotation_id: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LspResourceOperation {
    Create {
        path: PathBuf,
        overwrite: bool,
        ignore_if_exists: bool,
        annotation_id: Option<String>,
    },
    Rename {
        old_path: PathBuf,
        new_path: PathBuf,
        overwrite: bool,
        ignore_if_exists: bool,
        annotation_id: Option<String>,
    },
    Delete {
        path: PathBuf,
        recursive: bool,
        ignore_if_not_exists: bool,
        annotation_id: Option<String>,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LspWorkspaceOperation {
    Text(LspAnnotatedTextEdit),
    Resource(LspResourceOperation),
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct LspWorkspaceEdit {
    pub operations: Vec<LspWorkspaceOperation>,
    pub annotations: BTreeMap<String, LspChangeAnnotation>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LspRenameComparison {
    pub lsp_paths: Vec<PathBuf>,
    pub static_paths: Vec<PathBuf>,
    pub only_lsp: Vec<PathBuf>,
    pub only_static: Vec<PathBuf>,
    pub agrees: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LspCommand {
    pub command: String,
    pub title: String,
    pub arguments: Vec<Value>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LspCodeAction {
    pub title: String,
    pub kind: Option<String>,
    pub preferred: bool,
    pub disabled_reason: Option<String>,
    pub edit: Option<LspWorkspaceEdit>,
    pub command: Option<LspCommand>,
    pub raw: Value,
}

pub trait LspCommandPolicy {
    fn authorize(&self, command: &LspCommand) -> Result<(), String>;
    fn execute(&self, command: &LspCommand) -> Result<Value, String>;
}

pub fn prepare_rename(
    client: &mut LspClient,
    path: &Path,
    position: LspPosition,
) -> Result<LspCapabilityResult<Value>, LspError> {
    let result = client.request(
        "textDocument/prepareRename",
        json!({
            "textDocument": { "uri": file_uri(path) },
            "position": position,
        }),
    )?;
    Ok(if result.is_null() {
        LspCapabilityResult::Unsupported
    } else {
        LspCapabilityResult::Supported(result)
    })
}

pub fn rename(
    client: &mut LspClient,
    root: &Path,
    path: &Path,
    position: LspPosition,
    new_name: &str,
) -> Result<LspCapabilityResult<LspWorkspaceEdit>, LspError> {
    let result = client.request(
        "textDocument/rename",
        json!({
            "textDocument": { "uri": file_uri(path) },
            "position": position,
            "newName": new_name,
        }),
    )?;
    if result.is_null() {
        return Ok(LspCapabilityResult::Unsupported);
    }
    Ok(LspCapabilityResult::Supported(normalize_workspace_edit(
        root, &result,
    )?))
}

pub fn code_actions(
    client: &mut LspClient,
    root: &Path,
    path: &Path,
    range: LspRange,
    diagnostics: Vec<Value>,
    only: &[String],
) -> Result<LspCapabilityResult<Vec<LspCodeAction>>, LspError> {
    let result = client.request(
        "textDocument/codeAction",
        json!({
            "textDocument": { "uri": file_uri(path) },
            "range": range,
            "context": { "diagnostics": diagnostics, "only": only },
        }),
    )?;
    if result.is_null() {
        return Ok(LspCapabilityResult::Unsupported);
    }
    let actions = result
        .as_array()
        .ok_or_else(|| LspError::Protocol("code action response must be an array".to_owned()))?;
    actions
        .iter()
        .map(|value| normalize_code_action(root, value))
        .collect::<Result<Vec<_>, _>>()
        .map(LspCapabilityResult::Supported)
}

pub fn resolve_code_action(
    client: &mut LspClient,
    root: &Path,
    action: &LspCodeAction,
) -> Result<LspCapabilityResult<LspCodeAction>, LspError> {
    let result = client.request("codeAction/resolve", action.raw.clone())?;
    if result.is_null() {
        Ok(LspCapabilityResult::Unsupported)
    } else {
        normalize_code_action(root, &result).map(LspCapabilityResult::Supported)
    }
}

pub fn execute_command_guarded(
    policy: &impl LspCommandPolicy,
    command: &LspCommand,
) -> Result<Value, String> {
    policy.authorize(command)?;
    policy.execute(command)
}

pub fn compare_rename_paths(
    edit: &LspWorkspaceEdit,
    static_paths: &[PathBuf],
) -> LspRenameComparison {
    let mut lsp_paths = edit
        .operations
        .iter()
        .filter_map(|operation| match operation {
            LspWorkspaceOperation::Text(edit) => Some(edit.path.clone()),
            LspWorkspaceOperation::Resource(LspResourceOperation::Create { path, .. }) => {
                Some(path.clone())
            }
            LspWorkspaceOperation::Resource(LspResourceOperation::Rename {
                old_path,
                new_path,
                ..
            }) => Some(old_path.clone())
                .into_iter()
                .chain(Some(new_path.clone()))
                .next(),
            LspWorkspaceOperation::Resource(LspResourceOperation::Delete { path, .. }) => {
                Some(path.clone())
            }
        })
        .collect::<Vec<_>>();
    lsp_paths.sort();
    lsp_paths.dedup();
    let mut static_paths = static_paths.to_vec();
    static_paths.sort();
    static_paths.dedup();
    let only_lsp = lsp_paths
        .iter()
        .filter(|path| !static_paths.contains(path))
        .cloned()
        .collect();
    let only_static = static_paths
        .iter()
        .filter(|path| !lsp_paths.contains(path))
        .cloned()
        .collect();
    LspRenameComparison {
        agrees: lsp_paths == static_paths,
        lsp_paths,
        static_paths,
        only_lsp,
        only_static,
    }
}

pub fn normalize_workspace_edit(root: &Path, value: &Value) -> Result<LspWorkspaceEdit, LspError> {
    let mut edit = LspWorkspaceEdit::default();
    if let Some(annotations) = value.get("changeAnnotations").and_then(Value::as_object) {
        for (id, annotation) in annotations {
            edit.annotations.insert(
                id.clone(),
                LspChangeAnnotation {
                    label: annotation
                        .get("label")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                    needs_confirmation: annotation
                        .get("needsConfirmation")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                    description: annotation
                        .get("description")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                },
            );
        }
    }
    if let Some(changes) = value.get("changes").and_then(Value::as_object) {
        let mut uris = changes.keys().cloned().collect::<Vec<_>>();
        uris.sort();
        for uri in uris {
            if let Some(items) = changes.get(&uri).and_then(Value::as_array) {
                for item in items {
                    edit.operations
                        .push(LspWorkspaceOperation::Text(normalize_text_edit(
                            root, &uri, item,
                        )?));
                }
            }
        }
    }
    if let Some(changes) = value.get("documentChanges").and_then(Value::as_array) {
        for change in changes {
            if let Some(kind) = change.get("kind").and_then(Value::as_str) {
                edit.operations
                    .push(LspWorkspaceOperation::Resource(normalize_resource(
                        root, kind, change,
                    )?));
            } else {
                let uri = change
                    .pointer("/textDocument/uri")
                    .and_then(Value::as_str)
                    .ok_or_else(|| LspError::Protocol("document edit missing URI".to_owned()))?;
                for item in change
                    .get("edits")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    edit.operations
                        .push(LspWorkspaceOperation::Text(normalize_text_edit(
                            root, uri, item,
                        )?));
                }
            }
        }
    }
    Ok(edit)
}

fn normalize_code_action(root: &Path, value: &Value) -> Result<LspCodeAction, LspError> {
    if value.get("command").is_some() && value.get("title").is_none() {
        return Ok(LspCodeAction {
            title: value
                .get("title")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            kind: None,
            preferred: false,
            disabled_reason: None,
            edit: None,
            command: Some(normalize_command(value)),
            raw: value.clone(),
        });
    }
    Ok(LspCodeAction {
        title: value
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        kind: value.get("kind").and_then(Value::as_str).map(str::to_owned),
        preferred: value
            .get("isPreferred")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        disabled_reason: value
            .pointer("/disabled/reason")
            .and_then(Value::as_str)
            .map(str::to_owned),
        edit: value
            .get("edit")
            .map(|edit| normalize_workspace_edit(root, edit))
            .transpose()?,
        command: value.get("command").map(normalize_command),
        raw: value.clone(),
    })
}

fn normalize_command(value: &Value) -> LspCommand {
    let command = value.get("command").unwrap_or(value);
    LspCommand {
        command: command
            .get("command")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        title: command
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        arguments: command
            .get("arguments")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default(),
    }
}

fn normalize_text_edit(
    root: &Path,
    uri: &str,
    value: &Value,
) -> Result<LspAnnotatedTextEdit, LspError> {
    Ok(LspAnnotatedTextEdit {
        path: uri_to_relative(root, uri),
        range: serde_json::from_value(
            value
                .get("range")
                .cloned()
                .ok_or_else(|| LspError::Protocol("text edit missing range".to_owned()))?,
        )?,
        new_text: value
            .get("newText")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        annotation_id: value
            .get("annotationId")
            .and_then(Value::as_str)
            .map(str::to_owned),
    })
}

fn normalize_resource(
    root: &Path,
    kind: &str,
    value: &Value,
) -> Result<LspResourceOperation, LspError> {
    let annotation_id = value
        .get("annotationId")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let options = value.get("options").unwrap_or(&Value::Null);
    match kind {
        "create" => Ok(LspResourceOperation::Create {
            path: uri_to_relative(
                root,
                value.get("uri").and_then(Value::as_str).unwrap_or_default(),
            ),
            overwrite: options
                .get("overwrite")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            ignore_if_exists: options
                .get("ignoreIfExists")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            annotation_id,
        }),
        "rename" => Ok(LspResourceOperation::Rename {
            old_path: uri_to_relative(
                root,
                value
                    .get("oldUri")
                    .and_then(Value::as_str)
                    .unwrap_or_default(),
            ),
            new_path: uri_to_relative(
                root,
                value
                    .get("newUri")
                    .and_then(Value::as_str)
                    .unwrap_or_default(),
            ),
            overwrite: options
                .get("overwrite")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            ignore_if_exists: options
                .get("ignoreIfExists")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            annotation_id,
        }),
        "delete" => Ok(LspResourceOperation::Delete {
            path: uri_to_relative(
                root,
                value.get("uri").and_then(Value::as_str).unwrap_or_default(),
            ),
            recursive: options
                .get("recursive")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            ignore_if_not_exists: options
                .get("ignoreIfNotExists")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            annotation_id,
        }),
        _ => Err(LspError::Protocol(format!(
            "unsupported resource operation: {kind}"
        ))),
    }
}

fn uri_to_relative(root: &Path, uri: &str) -> PathBuf {
    let decoded = uri.strip_prefix("file://").unwrap_or(uri);
    let path = PathBuf::from(decoded);
    path.strip_prefix(root).unwrap_or(&path).to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_document_change_order_and_annotations() {
        let value = json!({
            "changeAnnotations": { "review": { "label": "Review", "needsConfirmation": true } },
            "documentChanges": [
                { "textDocument": { "uri": "file:///repo/src/lib.rs", "version": 1 }, "edits": [{ "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 0, "character": 3 } }, "newText": "new", "annotationId": "review" }] },
                { "kind": "create", "uri": "file:///repo/src/new.rs", "annotationId": "review" }
            ]
        });
        let edit = normalize_workspace_edit(Path::new("/repo"), &value).expect("edit");
        assert_eq!(edit.operations.len(), 2);
        assert!(edit.annotations["review"].needs_confirmation);
        assert!(matches!(edit.operations[0], LspWorkspaceOperation::Text(_)));
        assert!(matches!(
            edit.operations[1],
            LspWorkspaceOperation::Resource(_)
        ));
    }

    #[test]
    fn surfaces_static_lsp_disagreement() {
        let edit = LspWorkspaceEdit {
            operations: vec![LspWorkspaceOperation::Text(LspAnnotatedTextEdit {
                path: "src/lib.rs".into(),
                range: LspRange {
                    start: LspPosition {
                        line: 0,
                        character: 0,
                    },
                    end: LspPosition {
                        line: 0,
                        character: 1,
                    },
                },
                new_text: "x".into(),
                annotation_id: None,
            })],
            annotations: BTreeMap::new(),
        };
        let comparison = compare_rename_paths(&edit, &[PathBuf::from("src/main.rs")]);
        assert!(!comparison.agrees);
        assert_eq!(comparison.only_lsp, vec![PathBuf::from("src/lib.rs")]);
    }

    #[test]
    fn command_policy_must_authorize_before_execution() {
        struct Deny;
        impl LspCommandPolicy for Deny {
            fn authorize(&self, _: &LspCommand) -> Result<(), String> {
                Err("approval required".into())
            }
            fn execute(&self, _: &LspCommand) -> Result<Value, String> {
                panic!("must not execute")
            }
        }
        let command = LspCommand {
            command: "rust-analyzer.applySourceChange".into(),
            title: "Apply".into(),
            arguments: vec![],
        };
        assert_eq!(
            execute_command_guarded(&Deny, &command).unwrap_err(),
            "approval required"
        );
    }
}
