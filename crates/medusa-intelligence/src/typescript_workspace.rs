use std::{
    fmt, fs,
    path::{Component, Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use serde_json::json;
use walkdir::{DirEntry, WalkDir};

use crate::LspServerConfig;

const MAX_SUPPORTED_SOURCES: usize = 20_000;
const CONFIG_NAMES: [&str; 2] = ["tsconfig.json", "jsconfig.json"];

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TypeScriptWorkspace {
    pub repository_root: PathBuf,
    pub workspace_root: PathBuf,
    pub package_root: Option<PathBuf>,
    pub config_path: Option<PathBuf>,
    pub source_count: usize,
}

impl TypeScriptWorkspace {
    #[must_use]
    pub fn server_config(&self) -> LspServerConfig {
        LspServerConfig {
            language: "typescript_javascript".to_owned(),
            command: "typescript-language-server".to_owned(),
            args: vec!["--stdio".to_owned()],
            workspace_root: self.workspace_root.clone(),
            initialization_options: Some(json!({
                "hostInfo": "medusa",
                "preferences": {
                    "includeCompletionsForModuleExports": true,
                    "includeCompletionsWithInsertText": true
                }
            })),
        }
    }
}

#[derive(Debug)]
pub enum TypeScriptWorkspaceError {
    Io(std::io::Error),
    OutsideRepository(PathBuf),
    TooManySources { count: usize, limit: usize },
}

impl fmt::Display for TypeScriptWorkspaceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "TypeScript workspace I/O error: {error}"),
            Self::OutsideRepository(path) => write!(
                formatter,
                "TypeScript target is outside the repository: {}",
                path.display()
            ),
            Self::TooManySources { count, limit } => write!(
                formatter,
                "TypeScript workspace contains {count} supported source files, exceeding the limit of {limit}"
            ),
        }
    }
}

impl std::error::Error for TypeScriptWorkspaceError {}

impl From<std::io::Error> for TypeScriptWorkspaceError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

pub fn discover_typescript_workspace(
    repository_root: &Path,
    target: &Path,
) -> Result<TypeScriptWorkspace, TypeScriptWorkspaceError> {
    let repository_root = fs::canonicalize(repository_root)?;
    let target = canonicalize_existing_or_parent(target)?;
    if !target.starts_with(&repository_root) {
        return Err(TypeScriptWorkspaceError::OutsideRepository(target));
    }

    let start = if target.is_dir() {
        target.clone()
    } else {
        target.parent().unwrap_or(&repository_root).to_path_buf()
    };
    let config_path = nearest_named_file(&repository_root, &start, &CONFIG_NAMES);
    let package_root = nearest_named_file(&repository_root, &start, &["package.json"])
        .and_then(|path| path.parent().map(Path::to_path_buf));
    let workspace_root = config_path
        .as_ref()
        .and_then(|path| path.parent())
        .map(Path::to_path_buf)
        .or_else(|| package_root.clone())
        .unwrap_or_else(|| repository_root.clone());
    let source_count = count_supported_sources(&workspace_root)?;
    if source_count > MAX_SUPPORTED_SOURCES {
        return Err(TypeScriptWorkspaceError::TooManySources {
            count: source_count,
            limit: MAX_SUPPORTED_SOURCES,
        });
    }

    Ok(TypeScriptWorkspace {
        repository_root,
        workspace_root,
        package_root,
        config_path,
        source_count,
    })
}

fn canonicalize_existing_or_parent(path: &Path) -> Result<PathBuf, std::io::Error> {
    if path.exists() {
        return fs::canonicalize(path);
    }
    let mut current = path;
    while let Some(parent) = current.parent() {
        if parent.exists() {
            return fs::canonicalize(parent).map(|canonical| {
                path.strip_prefix(parent)
                    .map_or(canonical.clone(), |suffix| canonical.join(suffix))
            });
        }
        current = parent;
    }
    fs::canonicalize(path)
}

fn nearest_named_file(repository_root: &Path, start: &Path, names: &[&str]) -> Option<PathBuf> {
    let mut current = Some(start);
    while let Some(directory) = current {
        for name in names {
            let candidate = directory.join(name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
        if directory == repository_root {
            break;
        }
        current = directory
            .parent()
            .filter(|parent| parent.starts_with(repository_root));
    }
    None
}

fn count_supported_sources(root: &Path) -> Result<usize, TypeScriptWorkspaceError> {
    let mut count = 0usize;
    for entry in WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|entry| !ignored_entry(entry))
    {
        let entry = entry.map_err(|error| {
            TypeScriptWorkspaceError::Io(
                error
                    .into_io_error()
                    .unwrap_or_else(|| std::io::Error::other("walkdir error")),
            )
        })?;
        if entry.file_type().is_file() && supported_source(entry.path()) {
            count += 1;
            if count > MAX_SUPPORTED_SOURCES {
                return Ok(count);
            }
        }
    }
    Ok(count)
}

fn ignored_entry(entry: &DirEntry) -> bool {
    if entry.depth() == 0 {
        return false;
    }
    let name = entry.file_name().to_string_lossy();
    matches!(
        name.as_ref(),
        "node_modules"
            | ".git"
            | ".medusa"
            | "target"
            | "dist"
            | "build"
            | "coverage"
            | ".next"
            | ".turbo"
            | "out"
    ) || entry.path().components().any(|component| {
        matches!(
            component,
            Component::Normal(value) if value == "generated" || value == "vendor"
        )
    })
}

fn supported_source(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|extension| extension.to_str()),
        Some("ts" | "tsx" | "js" | "jsx" | "mts" | "cts" | "mjs" | "cjs")
    ) && !path
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.ends_with(".d.ts") || name.ends_with(".min.js"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, content: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parent");
        }
        fs::write(path, content).expect("write fixture");
    }

    #[test]
    fn selects_nearest_config_and_package_in_monorepo() {
        let repository = tempfile::tempdir().expect("repository");
        write(&repository.path().join("package.json"), "{}");
        write(&repository.path().join("tsconfig.json"), "{}");
        write(&repository.path().join("packages/app/package.json"), "{}");
        write(&repository.path().join("packages/app/tsconfig.json"), "{}");
        write(
            &repository.path().join("packages/app/src/main.ts"),
            "export const answer = 42;\n",
        );

        let workspace = discover_typescript_workspace(
            repository.path(),
            &repository.path().join("packages/app/src/main.ts"),
        )
        .expect("workspace");
        assert!(workspace.workspace_root.ends_with("packages/app"));
        assert!(
            workspace
                .config_path
                .expect("config")
                .ends_with("packages/app/tsconfig.json")
        );
        assert!(
            workspace
                .package_root
                .expect("package")
                .ends_with("packages/app")
        );
        assert_eq!(workspace.source_count, 1);
        assert_eq!(workspace.server_config().args, vec!["--stdio"]);
    }

    #[test]
    fn refuses_targets_outside_repository() {
        let repository = tempfile::tempdir().expect("repository");
        let outside = tempfile::tempdir().expect("outside");
        let error = discover_typescript_workspace(repository.path(), outside.path())
            .expect_err("outside target must fail");
        assert!(matches!(
            error,
            TypeScriptWorkspaceError::OutsideRepository(_)
        ));
    }

    #[test]
    fn excludes_dependencies_build_outputs_and_generated_files() {
        let repository = tempfile::tempdir().expect("repository");
        write(&repository.path().join("package.json"), "{}");
        write(&repository.path().join("src/main.ts"), "export {};\n");
        write(
            &repository.path().join("node_modules/pkg/index.ts"),
            "export {};\n",
        );
        write(&repository.path().join("dist/bundle.js"), "export {};\n");
        write(
            &repository.path().join("generated/client.ts"),
            "export {};\n",
        );
        write(&repository.path().join("src/types.d.ts"), "export {};\n");
        write(&repository.path().join("src/vendor.min.js"), "export {};\n");

        let workspace =
            discover_typescript_workspace(repository.path(), repository.path()).expect("workspace");
        assert_eq!(workspace.source_count, 1);
    }

    #[test]
    fn root_fallback_is_deterministic_without_config_or_package() {
        let repository = tempfile::tempdir().expect("repository");
        write(
            &repository.path().join("src/main.js"),
            "module.exports = {};\n",
        );
        let workspace = discover_typescript_workspace(
            repository.path(),
            &repository.path().join("src/main.js"),
        )
        .expect("workspace");
        assert_eq!(
            workspace.workspace_root,
            fs::canonicalize(repository.path()).expect("root")
        );
        assert!(workspace.config_path.is_none());
        assert!(workspace.package_root.is_none());
    }
}
