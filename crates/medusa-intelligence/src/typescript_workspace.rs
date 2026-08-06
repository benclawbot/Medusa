use std::{
    fmt, fs,
    path::{Component, Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use walkdir::{DirEntry, WalkDir};

use crate::LspServerConfig;

const MAX_SUPPORTED_SOURCES: usize = 20_000;
const CONFIG_NAMES: [&str; 2] = ["tsconfig.json", "jsconfig.json"];
const FINGERPRINT_VERSION: &[u8] = b"medusa-typescript-workspace-v1";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TypeScriptWorkspace {
    pub repository_root: PathBuf,
    pub workspace_root: PathBuf,
    pub package_root: Option<PathBuf>,
    pub config_path: Option<PathBuf>,
    pub source_count: usize,
    pub repository_fingerprint: String,
    pub workspace_fingerprint: String,
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

    pub fn refresh(&self) -> Result<Self, TypeScriptWorkspaceError> {
        discover_typescript_workspace(&self.repository_root, &self.workspace_root)
    }

    pub fn is_fresh(&self) -> Result<bool, TypeScriptWorkspaceError> {
        let refreshed = self.refresh()?;
        Ok(self.repository_fingerprint == refreshed.repository_fingerprint
            && self.workspace_fingerprint == refreshed.workspace_fingerprint)
    }

    #[must_use]
    pub fn same_repository(&self, other: &Self) -> bool {
        self.repository_fingerprint == other.repository_fingerprint
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
    let source_paths = collect_supported_sources(&workspace_root)?;
    let source_count = source_paths.len();
    if source_count > MAX_SUPPORTED_SOURCES {
        return Err(TypeScriptWorkspaceError::TooManySources {
            count: source_count,
            limit: MAX_SUPPORTED_SOURCES,
        });
    }
    let repository_fingerprint = fingerprint_repository(&repository_root);
    let workspace_fingerprint = fingerprint_workspace(
        &repository_root,
        &workspace_root,
        package_root.as_deref(),
        config_path.as_deref(),
        &source_paths,
        &repository_fingerprint,
    )?;

    Ok(TypeScriptWorkspace {
        repository_root,
        workspace_root,
        package_root,
        config_path,
        source_count,
        repository_fingerprint,
        workspace_fingerprint,
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

fn collect_supported_sources(root: &Path) -> Result<Vec<PathBuf>, TypeScriptWorkspaceError> {
    let mut sources = Vec::new();
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
            sources.push(entry.into_path());
            if sources.len() > MAX_SUPPORTED_SOURCES {
                return Ok(sources);
            }
        }
    }
    sources.sort_by_key(|path| normalized_path(root, path));
    Ok(sources)
}

fn fingerprint_repository(repository_root: &Path) -> String {
    let mut hasher = Sha256::new();
    hasher.update(FINGERPRINT_VERSION);
    hasher.update([0]);
    hasher.update(normalized_path(repository_root, repository_root).as_bytes());
    hasher.update([0]);
    hasher.update(repository_root.to_string_lossy().replace('\\', "/").as_bytes());
    hex::encode(hasher.finalize())
}

fn fingerprint_workspace(
    repository_root: &Path,
    workspace_root: &Path,
    package_root: Option<&Path>,
    config_path: Option<&Path>,
    source_paths: &[PathBuf],
    repository_fingerprint: &str,
) -> Result<String, TypeScriptWorkspaceError> {
    let mut hasher = Sha256::new();
    hasher.update(FINGERPRINT_VERSION);
    hasher.update([0]);
    hasher.update(repository_fingerprint.as_bytes());
    hash_path(&mut hasher, &normalized_path(repository_root, workspace_root));

    if let Some(config_path) = config_path {
        hash_file(&mut hasher, repository_root, config_path)?;
    }
    if let Some(package_root) = package_root {
        let package_json = package_root.join("package.json");
        if package_json.is_file() {
            hash_file(&mut hasher, repository_root, &package_json)?;
        }
    }
    for source_path in source_paths {
        hash_file(&mut hasher, repository_root, source_path)?;
    }

    Ok(hex::encode(hasher.finalize()))
}

fn hash_file(
    hasher: &mut Sha256,
    repository_root: &Path,
    path: &Path,
) -> Result<(), TypeScriptWorkspaceError> {
    hash_path(hasher, &normalized_path(repository_root, path));
    let content_digest = Sha256::digest(fs::read(path)?);
    hasher.update(content_digest);
    hasher.update([0]);
    Ok(())
}

fn hash_path(hasher: &mut Sha256, path: &str) {
    hasher.update(path.as_bytes());
    hasher.update([0]);
}

fn normalized_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
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
                .as_ref()
                .expect("config")
                .ends_with("packages/app/tsconfig.json")
        );
        assert!(
            workspace
                .package_root
                .as_ref()
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
        let original_fingerprint = workspace.workspace_fingerprint;
        write(
            &repository.path().join("generated/client.ts"),
            "export const changed = true;\n",
        );
        let refreshed =
            discover_typescript_workspace(repository.path(), repository.path()).expect("refresh");
        assert_eq!(refreshed.workspace_fingerprint, original_fingerprint);
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
        assert_eq!(workspace, workspace.refresh().expect("refresh"));
    }

    #[test]
    fn source_and_configuration_changes_invalidate_freshness() {
        let repository = tempfile::tempdir().expect("repository");
        write(&repository.path().join("package.json"), "{}");
        write(&repository.path().join("tsconfig.json"), "{}");
        write(
            &repository.path().join("src/main.ts"),
            "export const answer = 42;\n",
        );

        let workspace =
            discover_typescript_workspace(repository.path(), repository.path()).expect("workspace");
        assert!(workspace.is_fresh().expect("freshness"));
        write(
            &repository.path().join("src/main.ts"),
            "export const answer = 43;\n",
        );
        assert!(!workspace.is_fresh().expect("source freshness"));

        let refreshed = workspace.refresh().expect("refresh");
        write(
            &repository.path().join("tsconfig.json"),
            "{\"compilerOptions\":{\"strict\":true}}\n",
        );
        assert!(!refreshed.is_fresh().expect("configuration freshness"));
    }

    #[test]
    fn repository_switching_is_detected_even_with_identical_content() {
        let first = tempfile::tempdir().expect("first repository");
        let second = tempfile::tempdir().expect("second repository");
        for repository in [first.path(), second.path()] {
            write(&repository.join("package.json"), "{}");
            write(
                &repository.join("src/main.ts"),
                "export const answer = 42;\n",
            );
        }

        let first_workspace =
            discover_typescript_workspace(first.path(), first.path()).expect("first workspace");
        let second_workspace =
            discover_typescript_workspace(second.path(), second.path()).expect("second workspace");
        assert!(!first_workspace.same_repository(&second_workspace));
        assert_ne!(
            first_workspace.repository_fingerprint,
            second_workspace.repository_fingerprint
        );
        assert_ne!(
            first_workspace.workspace_fingerprint,
            second_workspace.workspace_fingerprint
        );
    }

    #[test]
    fn large_workspace_fingerprint_is_deterministic() {
        let repository = tempfile::tempdir().expect("repository");
        write(&repository.path().join("package.json"), "{}");
        for index in 0..512 {
            write(
                &repository.path().join(format!("src/module-{index:04}.ts")),
                &format!("export const value{index} = {index};\n"),
            );
        }

        let first =
            discover_typescript_workspace(repository.path(), repository.path()).expect("first");
        let second =
            discover_typescript_workspace(repository.path(), repository.path()).expect("second");
        assert_eq!(first.source_count, 512);
        assert_eq!(first.workspace_fingerprint, second.workspace_fingerprint);
    }
}
