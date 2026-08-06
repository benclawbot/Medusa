use std::{
    fmt,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{TypeScriptWorkspace, discover_typescript_workspace};

const ADAPTER_SOURCE: &str = include_str!("../../../tools/typescript-semantic-adapter.mjs");

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TypeScriptSemanticOperation {
    Definition,
    References,
    Diagnostics,
    WorkspaceSymbols,
    Rename,
}

impl TypeScriptSemanticOperation {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Definition => "definition",
            Self::References => "references",
            Self::Diagnostics => "diagnostics",
            Self::WorkspaceSymbols => "workspace_symbols",
            Self::Rename => "rename",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TypeScriptSemanticRequest {
    pub operation: TypeScriptSemanticOperation,
    pub path: Option<PathBuf>,
    pub line: Option<u32>,
    pub character: Option<u32>,
    pub query: Option<String>,
    pub new_name: Option<String>,
    pub expected_workspace_fingerprint: Option<String>,
}

#[derive(Debug)]
pub enum TypeScriptSemanticError {
    Workspace(String),
    DependencyUnavailable(String),
    Io(std::io::Error),
    Json(serde_json::Error),
    Adapter { code: String, message: String },
    Protocol(String),
}

impl fmt::Display for TypeScriptSemanticError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Workspace(message) => write!(formatter, "TypeScript workspace error: {message}"),
            Self::DependencyUnavailable(message) => formatter.write_str(message),
            Self::Io(error) => write!(formatter, "TypeScript adapter I/O error: {error}"),
            Self::Json(error) => write!(formatter, "TypeScript adapter JSON error: {error}"),
            Self::Adapter { code, message } => write!(formatter, "TypeScript adapter {code}: {message}"),
            Self::Protocol(message) => write!(formatter, "TypeScript adapter protocol error: {message}"),
        }
    }
}

impl std::error::Error for TypeScriptSemanticError {}
impl From<std::io::Error> for TypeScriptSemanticError { fn from(error: std::io::Error) -> Self { Self::Io(error) } }
impl From<serde_json::Error> for TypeScriptSemanticError { fn from(error: serde_json::Error) -> Self { Self::Json(error) } }

pub fn execute_typescript_semantic(
    repository_root: &Path,
    request: &TypeScriptSemanticRequest,
) -> Result<Value, TypeScriptSemanticError> {
    let target = request
        .path
        .as_ref()
        .map(|path| repository_root.join(path))
        .unwrap_or_else(|| repository_root.to_path_buf());
    let workspace = discover_typescript_workspace(repository_root, &target)
        .map_err(|error| TypeScriptSemanticError::Workspace(error.to_string()))?;
    execute_with_workspace(&workspace, request)
}

fn execute_with_workspace(
    workspace: &TypeScriptWorkspace,
    request: &TypeScriptSemanticRequest,
) -> Result<Value, TypeScriptSemanticError> {
    let mut child = Command::new("node")
        .args(["--input-type=module", "--eval", ADAPTER_SOURCE])
        .current_dir(&workspace.repository_root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                TypeScriptSemanticError::DependencyUnavailable(
                    "Node.js is required for TypeScript/JavaScript semantic intelligence".to_owned(),
                )
            } else {
                TypeScriptSemanticError::Io(error)
            }
        })?;

    let payload = json!({
        "operation": request.operation.as_str(),
        "repository_root": workspace.repository_root,
        "workspace_root": workspace.workspace_root,
        "package_root": workspace.package_root,
        "config_path": workspace.config_path,
        "path": request.path,
        "line": request.line,
        "character": request.character,
        "query": request.query,
        "new_name": request.new_name,
        "expected_workspace_fingerprint": request.expected_workspace_fingerprint,
    });
    child
        .stdin
        .as_mut()
        .ok_or_else(|| TypeScriptSemanticError::Protocol("adapter stdin is unavailable".to_owned()))?
        .write_all(serde_json::to_string(&payload)?.as_bytes())?;
    drop(child.stdin.take());
    let output = child.wait_with_output()?;
    let response: Value = serde_json::from_slice(&output.stdout).map_err(|error| {
        TypeScriptSemanticError::Protocol(format!(
            "adapter returned invalid JSON ({error}); stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        ))
    })?;
    if let Some(error) = response.get("error") {
        return Err(TypeScriptSemanticError::Adapter {
            code: error.get("code").and_then(Value::as_str).unwrap_or("unknown").to_owned(),
            message: error.get("message").and_then(Value::as_str).unwrap_or("adapter failed").to_owned(),
        });
    }
    if !output.status.success() {
        return Err(TypeScriptSemanticError::Protocol(format!(
            "adapter exited with {}; stderr: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    if response.get("evidence").is_none() || response.get("result").is_none() {
        return Err(TypeScriptSemanticError::Protocol(
            "adapter response is missing evidence or result".to_owned(),
        ));
    }
    Ok(response)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    fn write(path: &Path, content: &str) {
        if let Some(parent) = path.parent() { fs::create_dir_all(parent).expect("create parent"); }
        fs::write(path, content).expect("write fixture");
    }

    #[test]
    fn adapter_definitions_references_diagnostics_symbols_and_rename() {
        if Command::new("node").arg("--version").output().is_err() { return; }
        let repo = tempfile::tempdir().expect("repo");
        write(&repo.path().join("package.json"), "{\"devDependencies\":{\"typescript\":\"*\"}}");
        write(&repo.path().join("tsconfig.json"), "{\"compilerOptions\":{\"strict\":true},\"include\":[\"src/**/*.ts\"]}");
        write(&repo.path().join("src/lib.ts"), "export function greet(name: string) { return name.toUpperCase(); }\n");
        write(&repo.path().join("src/main.ts"), "import { greet } from './lib';\nconst value: number = 'bad';\nconsole.log(greet('Medusa'));\n");

        let definition = match execute_typescript_semantic(repo.path(), &TypeScriptSemanticRequest {
            operation: TypeScriptSemanticOperation::Definition,
            path: Some("src/main.ts".into()), line: Some(2), character: Some(14), query: None,
            new_name: None, expected_workspace_fingerprint: None,
        }) {
            Ok(value) => value,
            Err(TypeScriptSemanticError::Adapter { code, .. }) if code == "typescript_dependency_unavailable" => return,
            Err(error) => panic!("definition failed: {error}"),
        };
        assert!(definition["result"].as_array().is_some_and(|items| !items.is_empty()));

        let references = execute_typescript_semantic(repo.path(), &TypeScriptSemanticRequest {
            operation: TypeScriptSemanticOperation::References,
            path: Some("src/main.ts".into()), line: Some(2), character: Some(14), query: None,
            new_name: None, expected_workspace_fingerprint: None,
        }).expect("references");
        assert!(references["result"].as_array().is_some_and(|items| items.len() >= 2));

        let diagnostics = execute_typescript_semantic(repo.path(), &TypeScriptSemanticRequest {
            operation: TypeScriptSemanticOperation::Diagnostics,
            path: Some("src/main.ts".into()), line: None, character: None, query: None,
            new_name: None, expected_workspace_fingerprint: None,
        }).expect("diagnostics");
        assert!(diagnostics["result"].as_array().is_some_and(|items| !items.is_empty()));

        let symbols = execute_typescript_semantic(repo.path(), &TypeScriptSemanticRequest {
            operation: TypeScriptSemanticOperation::WorkspaceSymbols,
            path: None, line: None, character: None, query: Some("greet".to_owned()),
            new_name: None, expected_workspace_fingerprint: None,
        }).expect("workspace symbols");
        assert!(symbols["result"].as_array().is_some_and(|items| !items.is_empty()));

        let fingerprint = definition["evidence"]["workspace_fingerprint"]
            .as_str().expect("fingerprint").to_owned();
        write(&repo.path().join("src/main.ts"), "import { greet } from './lib';\nconsole.log(greet('changed'));\n");
        let stale = execute_typescript_semantic(repo.path(), &TypeScriptSemanticRequest {
            operation: TypeScriptSemanticOperation::References,
            path: Some("src/main.ts".into()), line: Some(1), character: Some(14), query: None,
            new_name: None, expected_workspace_fingerprint: Some(fingerprint),
        }).expect_err("stale workspace must fail");
        assert!(matches!(stale, TypeScriptSemanticError::Adapter { code, .. } if code == "stale_workspace"));

        write(&repo.path().join("src/main.ts"), "import { greet } from './lib';\nconst value: number = 'bad';\nconsole.log(greet('Medusa'));\n");
        let rename = execute_typescript_semantic(repo.path(), &TypeScriptSemanticRequest {
            operation: TypeScriptSemanticOperation::Rename,
            path: Some("src/lib.ts".into()), line: Some(0), character: Some(17), query: None,
            new_name: Some("welcome".to_owned()), expected_workspace_fingerprint: None,
        }).expect("rename");
        assert!(rename["result"]["edits"].as_array().is_some_and(|items| items.len() >= 2));
    }
}
