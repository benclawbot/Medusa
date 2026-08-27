use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    time::Instant,
};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub use crate::lsp_navigation::{LspPosition, LspRange};
use crate::{LspClient, LspError};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LspHover {
    pub rendered: String,
    pub structured: Value,
    pub range: Option<LspRange>,
    pub latency_ms: u128,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticSeverity {
    Error,
    Warning,
    Information,
    Hint,
    Unknown,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LspRelatedDiagnostic {
    pub path: PathBuf,
    pub range: LspRange,
    pub message: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LspDiagnostic {
    pub path: PathBuf,
    pub range: LspRange,
    pub severity: DiagnosticSeverity,
    pub code: Option<String>,
    pub source: Option<String>,
    pub message: String,
    pub related: Vec<LspRelatedDiagnostic>,
    pub document_version: Option<i64>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct DiagnosticSnapshot {
    versions: BTreeMap<PathBuf, Option<i64>>,
    diagnostics: BTreeMap<PathBuf, Vec<LspDiagnostic>>,
}

impl DiagnosticSnapshot {
    pub fn ingest_publish(
        &mut self,
        workspace_root: &Path,
        params: &Value,
    ) -> Result<(), LspError> {
        let uri = params
            .get("uri")
            .and_then(Value::as_str)
            .ok_or_else(|| LspError::Protocol("publishDiagnostics missing uri".to_owned()))?;
        let path = repository_path(workspace_root, uri);
        let version = params.get("version").and_then(Value::as_i64);
        let diagnostics = params
            .get("diagnostics")
            .and_then(Value::as_array)
            .ok_or_else(|| LspError::Protocol("publishDiagnostics missing diagnostics".to_owned()))?
            .iter()
            .map(|value| normalize_diagnostic(workspace_root, &path, version, value))
            .collect::<Result<Vec<_>, _>>()?;
        self.versions.insert(path.clone(), version);
        self.diagnostics.insert(path, diagnostics);
        Ok(())
    }

    #[must_use]
    pub fn current_for(&self, path: &Path, document_version: Option<i64>) -> Vec<LspDiagnostic> {
        match self.versions.get(path) {
            Some(version) if *version == document_version || document_version.is_none() => {
                self.diagnostics.get(path).cloned().unwrap_or_default()
            }
            _ => Vec::new(),
        }
    }

    #[must_use]
    pub fn by_severity(&self, severity: DiagnosticSeverity) -> Vec<LspDiagnostic> {
        let mut results: Vec<_> = self
            .diagnostics
            .values()
            .flatten()
            .filter(|diagnostic| diagnostic.severity == severity)
            .cloned()
            .collect();
        sort_diagnostics(&mut results);
        results
    }

    #[must_use]
    pub fn intersecting(
        &self,
        path: &Path,
        range: LspRange,
        document_version: Option<i64>,
    ) -> Vec<LspDiagnostic> {
        let mut results: Vec<_> = self
            .current_for(path, document_version)
            .into_iter()
            .filter(|diagnostic| ranges_intersect(diagnostic.range, range))
            .collect();
        sort_diagnostics(&mut results);
        results
    }

    #[must_use]
    pub fn verification_paths(&self, previous: &Self) -> Vec<PathBuf> {
        let mut paths = Vec::new();
        for (path, diagnostics) in &self.diagnostics {
            if previous.diagnostics.get(path) != Some(diagnostics) {
                paths.push(path.clone());
            }
        }
        paths.sort();
        paths.dedup();
        paths
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SemanticToken {
    pub range: LspRange,
    pub token_type: u32,
    pub token_modifiers: u32,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct SemanticTokenState {
    pub result_id: Option<String>,
    pub encoded: Vec<u32>,
    pub tokens: Vec<SemanticToken>,
}

impl SemanticTokenState {
    pub fn replace_full(&mut self, response: &Value) -> Result<(), LspError> {
        self.result_id = response
            .get("resultId")
            .and_then(Value::as_str)
            .map(str::to_owned);
        self.encoded = parse_u32_array(response.get("data").unwrap_or(&Value::Null))?;
        self.tokens = decode_semantic_tokens(&self.encoded)?;
        Ok(())
    }

    pub fn apply_delta(&mut self, response: &Value) -> Result<(), LspError> {
        let edits = response
            .get("edits")
            .and_then(Value::as_array)
            .ok_or_else(|| LspError::Protocol("semantic token delta missing edits".to_owned()))?;
        let mut offset: i64 = 0;
        for edit in edits {
            let start =
                edit.get("start").and_then(Value::as_u64).ok_or_else(|| {
                    LspError::Protocol("semantic token edit missing start".to_owned())
                })? as i64
                    + offset;
            let delete_count = edit
                .get("deleteCount")
                .and_then(Value::as_u64)
                .ok_or_else(|| {
                    LspError::Protocol("semantic token edit missing deleteCount".to_owned())
                })? as usize;
            let data = match edit.get("data") {
                Some(value) => parse_u32_array(value)?,
                None => Vec::new(),
            };
            if start < 0
                || start as usize > self.encoded.len()
                || start as usize + delete_count > self.encoded.len()
            {
                return Err(LspError::Protocol(
                    "semantic token delta is out of bounds".to_owned(),
                ));
            }
            self.encoded.splice(
                start as usize..start as usize + delete_count,
                data.iter().copied(),
            );
            offset += data.len() as i64 - delete_count as i64;
        }
        self.result_id = response
            .get("resultId")
            .and_then(Value::as_str)
            .map(str::to_owned);
        self.tokens = decode_semantic_tokens(&self.encoded)?;
        Ok(())
    }
}

impl LspClient {
    pub fn hover(
        &mut self,
        workspace_root: &Path,
        path: &Path,
        position: LspPosition,
    ) -> Result<Option<LspHover>, LspError> {
        let started = Instant::now();
        let response = self.request(
            "textDocument/hover",
            json!({
                "textDocument": { "uri": file_uri(&workspace_root.join(path)) },
                "position": position,
            }),
        )?;
        if response.is_null() {
            return Ok(None);
        }
        let contents = response.get("contents").cloned().unwrap_or(Value::Null);
        Ok(Some(LspHover {
            rendered: render_markup(&contents),
            structured: contents,
            range: response.get("range").map(parse_range).transpose()?,
            latency_ms: started.elapsed().as_millis(),
        }))
    }

    pub fn pull_diagnostics(
        &mut self,
        workspace_root: &Path,
        path: &Path,
        previous_result_id: Option<&str>,
    ) -> Result<Value, LspError> {
        self.request(
            "textDocument/diagnostic",
            json!({
                "textDocument": { "uri": file_uri(&workspace_root.join(path)) },
                "previousResultId": previous_result_id,
            }),
        )
    }

    pub fn semantic_tokens_full(
        &mut self,
        workspace_root: &Path,
        path: &Path,
    ) -> Result<Value, LspError> {
        self.request(
            "textDocument/semanticTokens/full",
            json!({
                "textDocument": { "uri": file_uri(&workspace_root.join(path)) }
            }),
        )
    }

    pub fn semantic_tokens_delta(
        &mut self,
        workspace_root: &Path,
        path: &Path,
        previous_result_id: &str,
    ) -> Result<Value, LspError> {
        self.request(
            "textDocument/semanticTokens/full/delta",
            json!({
                "textDocument": { "uri": file_uri(&workspace_root.join(path)) },
                "previousResultId": previous_result_id,
            }),
        )
    }
}

fn normalize_diagnostic(
    root: &Path,
    path: &Path,
    version: Option<i64>,
    value: &Value,
) -> Result<LspDiagnostic, LspError> {
    let related = value
        .get("relatedInformation")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|item| {
            let location = item.get("location").ok_or_else(|| {
                LspError::Protocol("related diagnostic missing location".to_owned())
            })?;
            Ok(LspRelatedDiagnostic {
                path: repository_path(
                    root,
                    location
                        .get("uri")
                        .and_then(Value::as_str)
                        .unwrap_or_default(),
                ),
                range: parse_range(location.get("range").unwrap_or(&Value::Null))?,
                message: item
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
            })
        })
        .collect::<Result<Vec<_>, LspError>>()?;
    Ok(LspDiagnostic {
        path: path.to_path_buf(),
        range: parse_range(value.get("range").unwrap_or(&Value::Null))?,
        severity: match value.get("severity").and_then(Value::as_u64) {
            Some(1) => DiagnosticSeverity::Error,
            Some(2) => DiagnosticSeverity::Warning,
            Some(3) => DiagnosticSeverity::Information,
            Some(4) => DiagnosticSeverity::Hint,
            _ => DiagnosticSeverity::Unknown,
        },
        code: value.get("code").map(|code| {
            code.as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| code.to_string())
        }),
        source: value
            .get("source")
            .and_then(Value::as_str)
            .map(str::to_owned),
        message: value
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        related,
        document_version: version,
    })
}

fn parse_range(value: &Value) -> Result<LspRange, LspError> {
    Ok(LspRange {
        start: parse_position(value.get("start").unwrap_or(&Value::Null))?,
        end: parse_position(value.get("end").unwrap_or(&Value::Null))?,
    })
}

fn parse_position(value: &Value) -> Result<LspPosition, LspError> {
    Ok(LspPosition {
        line: value
            .get("line")
            .and_then(Value::as_u64)
            .ok_or_else(|| LspError::Protocol("position missing line".to_owned()))?
            as u32,
        character: value
            .get("character")
            .and_then(Value::as_u64)
            .ok_or_else(|| LspError::Protocol("position missing character".to_owned()))?
            as u32,
    })
}

fn parse_u32_array(value: &Value) -> Result<Vec<u32>, LspError> {
    value
        .as_array()
        .ok_or_else(|| LspError::Protocol("semantic token data is not an array".to_owned()))?
        .iter()
        .map(|item| {
            item.as_u64()
                .map(|number| number as u32)
                .ok_or_else(|| LspError::Protocol("invalid semantic token integer".to_owned()))
        })
        .collect()
}

fn decode_semantic_tokens(data: &[u32]) -> Result<Vec<SemanticToken>, LspError> {
    if !data.len().is_multiple_of(5) {
        return Err(LspError::Protocol(
            "semantic token data length is not divisible by five".to_owned(),
        ));
    }
    let mut line = 0;
    let mut character = 0;
    let mut tokens = Vec::new();
    for chunk in data.chunks_exact(5) {
        line += chunk[0];
        character = if chunk[0] == 0 {
            character + chunk[1]
        } else {
            chunk[1]
        };
        tokens.push(SemanticToken {
            range: LspRange {
                start: LspPosition { line, character },
                end: LspPosition {
                    line,
                    character: character + chunk[2],
                },
            },
            token_type: chunk[3],
            token_modifiers: chunk[4],
        });
    }
    Ok(tokens)
}

fn render_markup(contents: &Value) -> String {
    match contents {
        Value::String(text) => text.clone(),
        Value::Array(items) => items
            .iter()
            .map(render_markup)
            .collect::<Vec<_>>()
            .join("\n\n"),
        Value::Object(map) => map
            .get("value")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        _ => String::new(),
    }
}

fn ranges_intersect(left: LspRange, right: LspRange) -> bool {
    left.start <= right.end && right.start <= left.end
}
fn sort_diagnostics(items: &mut [LspDiagnostic]) {
    items.sort_by(|a, b| (&a.path, a.range, &a.message).cmp(&(&b.path, b.range, &b.message)));
}
fn file_uri(path: &Path) -> String {
    let normalized = path.to_string_lossy().replace('\\', "/");
    if normalized.starts_with('/') {
        format!("file://{normalized}")
    } else {
        format!("file:///{normalized}")
    }
}
fn repository_path(root: &Path, uri: &str) -> PathBuf {
    let raw = uri.strip_prefix("file://").unwrap_or(uri);
    let absolute = PathBuf::from(raw);
    absolute
        .strip_prefix(root)
        .unwrap_or(&absolute)
        .to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stale_diagnostics_are_hidden() {
        let mut snapshot = DiagnosticSnapshot::default();
        snapshot.ingest_publish(Path::new("/repo"), &json!({"uri":"file:///repo/src/lib.rs","version":2,"diagnostics":[{"range":{"start":{"line":1,"character":0},"end":{"line":1,"character":3}},"severity":1,"message":"broken"}]})).expect("publish");
        assert_eq!(
            snapshot.current_for(Path::new("src/lib.rs"), Some(2)).len(),
            1
        );
        assert!(
            snapshot
                .current_for(Path::new("src/lib.rs"), Some(3))
                .is_empty()
        );
    }

    #[test]
    fn semantic_delta_matches_full_refresh() {
        let mut delta = SemanticTokenState::default();
        delta
            .replace_full(&json!({"resultId":"a","data":[0,0,3,1,0]}))
            .expect("full");
        delta
            .apply_delta(&json!({"resultId":"b","edits":[{"start":2,"deleteCount":1,"data":[5]}]}))
            .expect("delta");
        let mut full = SemanticTokenState::default();
        full.replace_full(&json!({"resultId":"b","data":[0,0,5,1,0]}))
            .expect("full");
        assert_eq!(delta, full);
    }

    #[test]
    fn diagnostic_changes_narrow_verification_paths() {
        let previous = DiagnosticSnapshot::default();
        let mut current = DiagnosticSnapshot::default();
        current
            .ingest_publish(
                Path::new("/repo"),
                &json!({"uri":"file:///repo/src/main.rs","diagnostics":[]}),
            )
            .expect("publish");
        assert_eq!(
            current.verification_paths(&previous),
            vec![PathBuf::from("src/main.rs")]
        );
    }

    #[test]
    fn markup_content_is_rendered_deterministically() {
        assert_eq!(
            render_markup(&json!([{"kind":"markdown","value":"`u32`"}, "docs"])),
            "`u32`\n\ndocs"
        );
    }
}
