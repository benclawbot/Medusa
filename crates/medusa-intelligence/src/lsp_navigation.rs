use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    time::Instant,
};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{LspClient, LspError};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LspNavigationKind {
    Definition,
    Declaration,
    Reference,
    DocumentSymbol,
    WorkspaceSymbol,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Ord, PartialOrd, Serialize)]
pub struct LspPosition {
    pub line: u32,
    pub character: u32,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Ord, PartialOrd, Serialize)]
pub struct LspRange {
    pub start: LspPosition,
    pub end: LspPosition,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Ord, PartialOrd, Serialize)]
pub struct LspLocation {
    pub path: PathBuf,
    pub range: LspRange,
    pub name: Option<String>,
    pub kind: Option<u32>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LspNavigationResult {
    pub kind: LspNavigationKind,
    pub locations: Vec<LspLocation>,
    pub source: String,
    pub confidence_millis: u16,
    pub latency_ms: u64,
    pub unsupported: bool,
    pub disagreement: Option<String>,
}

impl LspNavigationResult {
    fn unsupported(kind: LspNavigationKind, latency_ms: u64) -> Self {
        Self {
            kind,
            locations: Vec::new(),
            source: "lsp".to_owned(),
            confidence_millis: 0,
            latency_ms,
            unsupported: true,
            disagreement: None,
        }
    }
}

pub fn go_to_definition(
    client: &mut LspClient,
    workspace_root: &Path,
    path: &Path,
    line: u32,
    character: u32,
) -> Result<LspNavigationResult, LspError> {
    position_query(
        client,
        workspace_root,
        path,
        line,
        character,
        "textDocument/definition",
        LspNavigationKind::Definition,
    )
}

pub fn go_to_declaration(
    client: &mut LspClient,
    workspace_root: &Path,
    path: &Path,
    line: u32,
    character: u32,
) -> Result<LspNavigationResult, LspError> {
    position_query(
        client,
        workspace_root,
        path,
        line,
        character,
        "textDocument/declaration",
        LspNavigationKind::Declaration,
    )
}

pub fn find_references(
    client: &mut LspClient,
    workspace_root: &Path,
    path: &Path,
    line: u32,
    character: u32,
    include_declaration: bool,
) -> Result<LspNavigationResult, LspError> {
    let started = Instant::now();
    let response = client.request(
        "textDocument/references",
        json!({
            "textDocument": { "uri": file_uri(path) },
            "position": { "line": line, "character": character },
            "context": { "includeDeclaration": include_declaration }
        }),
    )?;
    Ok(normalize_response(
        LspNavigationKind::Reference,
        workspace_root,
        response,
        started.elapsed().as_millis() as u64,
    ))
}

pub fn document_symbols(
    client: &mut LspClient,
    workspace_root: &Path,
    path: &Path,
) -> Result<LspNavigationResult, LspError> {
    let started = Instant::now();
    let response = client.request(
        "textDocument/documentSymbol",
        json!({
            "textDocument": { "uri": file_uri(path) }
        }),
    )?;
    Ok(normalize_response(
        LspNavigationKind::DocumentSymbol,
        workspace_root,
        response,
        started.elapsed().as_millis() as u64,
    ))
}

pub fn workspace_symbols(
    client: &mut LspClient,
    workspace_root: &Path,
    query: &str,
) -> Result<LspNavigationResult, LspError> {
    let started = Instant::now();
    let response = client.request("workspace/symbol", json!({ "query": query }))?;
    Ok(normalize_response(
        LspNavigationKind::WorkspaceSymbol,
        workspace_root,
        response,
        started.elapsed().as_millis() as u64,
    ))
}

pub fn compare_with_static(
    result: &mut LspNavigationResult,
    static_paths: impl IntoIterator<Item = PathBuf>,
) {
    let lsp: BTreeSet<_> = result
        .locations
        .iter()
        .map(|location| location.path.clone())
        .collect();
    let static_set: BTreeSet<_> = static_paths.into_iter().collect();
    if lsp != static_set {
        result.disagreement = Some(format!(
            "LSP paths {lsp:?} differ from static paths {static_set:?}"
        ));
        result.confidence_millis = 700;
    }
}

fn position_query(
    client: &mut LspClient,
    workspace_root: &Path,
    path: &Path,
    line: u32,
    character: u32,
    method: &str,
    kind: LspNavigationKind,
) -> Result<LspNavigationResult, LspError> {
    let started = Instant::now();
    let response = client.request(
        method,
        json!({
            "textDocument": { "uri": file_uri(path) },
            "position": { "line": line, "character": character }
        }),
    )?;
    Ok(normalize_response(
        kind,
        workspace_root,
        response,
        started.elapsed().as_millis() as u64,
    ))
}

fn normalize_response(
    kind: LspNavigationKind,
    root: &Path,
    response: Value,
    latency_ms: u64,
) -> LspNavigationResult {
    if response.is_null() {
        return LspNavigationResult::unsupported(kind, latency_ms);
    }
    let mut locations = Vec::new();
    collect_locations(root, &response, None, None, &mut locations);
    locations.sort();
    locations.dedup();
    LspNavigationResult {
        kind,
        confidence_millis: if locations.is_empty() { 500 } else { 950 },
        locations,
        source: "lsp".to_owned(),
        latency_ms,
        unsupported: false,
        disagreement: None,
    }
}

fn collect_locations(
    root: &Path,
    value: &Value,
    inherited_name: Option<String>,
    inherited_kind: Option<u32>,
    out: &mut Vec<LspLocation>,
) {
    if let Some(items) = value.as_array() {
        for item in items {
            collect_locations(root, item, inherited_name.clone(), inherited_kind, out);
        }
        return;
    }
    let Some(object) = value.as_object() else {
        return;
    };
    let name = object
        .get("name")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or(inherited_name);
    let symbol_kind = object
        .get("kind")
        .and_then(Value::as_u64)
        .map(|kind| kind as u32)
        .or(inherited_kind);
    if let Some(location) = object.get("location") {
        collect_locations(root, location, name.clone(), symbol_kind, out);
    }
    let uri = object
        .get("uri")
        .or_else(|| object.get("targetUri"))
        .and_then(Value::as_str);
    let range = object
        .get("range")
        .or_else(|| object.get("targetSelectionRange"));
    if let (Some(uri), Some(range)) = (uri, range) {
        if let Some(range) = parse_range(range) {
            out.push(LspLocation {
                path: repository_path(root, uri),
                range,
                name: name.clone(),
                kind: symbol_kind,
            });
        }
    }
    if let Some(children) = object.get("children") {
        collect_locations(root, children, name, symbol_kind, out);
    }
}

fn parse_range(value: &Value) -> Option<LspRange> {
    Some(LspRange {
        start: parse_position(value.get("start")?)?,
        end: parse_position(value.get("end")?)?,
    })
}

fn parse_position(value: &Value) -> Option<LspPosition> {
    Some(LspPosition {
        line: value.get("line")?.as_u64()? as u32,
        character: value.get("character")?.as_u64()? as u32,
    })
}

fn repository_path(root: &Path, uri: &str) -> PathBuf {
    let decoded = uri
        .strip_prefix("file://")
        .unwrap_or(uri)
        .replace("%20", " ");
    let absolute = PathBuf::from(decoded);
    absolute
        .strip_prefix(root)
        .unwrap_or(&absolute)
        .to_path_buf()
}

fn file_uri(path: &Path) -> String {
    let normalized = path
        .to_string_lossy()
        .replace('\\', "/")
        .replace(' ', "%20");
    if normalized.starts_with('/') {
        format!("file://{normalized}")
    } else {
        format!("file:///{normalized}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_location_links_and_sorts_deterministically() {
        let response = json!([
            {"targetUri":"file:///repo/src/z.rs","targetSelectionRange":{"start":{"line":2,"character":1},"end":{"line":2,"character":4}}},
            {"uri":"file:///repo/src/a.rs","range":{"start":{"line":1,"character":0},"end":{"line":1,"character":3}}}
        ]);
        let result = normalize_response(
            LspNavigationKind::Definition,
            Path::new("/repo"),
            response,
            3,
        );
        assert_eq!(result.locations[0].path, PathBuf::from("src/a.rs"));
        assert_eq!(result.locations[1].path, PathBuf::from("src/z.rs"));
    }

    #[test]
    fn records_static_disagreement() {
        let mut result = normalize_response(
            LspNavigationKind::Definition,
            Path::new("/repo"),
            json!({"uri":"file:///repo/src/lib.rs","range":{"start":{"line":0,"character":0},"end":{"line":0,"character":2}}}),
            1,
        );
        compare_with_static(&mut result, [PathBuf::from("src/other.rs")]);
        assert!(result.disagreement.is_some());
        assert_eq!(result.confidence_millis, 700);
    }

    #[test]
    fn null_is_typed_as_unsupported() {
        let result = normalize_response(
            LspNavigationKind::WorkspaceSymbol,
            Path::new("/repo"),
            Value::Null,
            0,
        );
        assert!(result.unsupported);
    }
}
