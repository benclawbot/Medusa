use std::{
    collections::BTreeSet,
    fs,
    path::{Component, Path},
};

use serde::{Deserialize, Serialize};

use crate::{EvidenceError, Result, fingerprint, hash_bytes};

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKind {
    Added,
    Modified,
    Deleted,
    Renamed,
    Copied,
    TypeChanged,
    Unmerged,
    Unknown,
}

#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct ChangedComponent {
    pub kind: ChangeKind,
    pub path: String,
    pub previous_path: Option<String>,
    pub generated: bool,
    pub package_owner: Option<String>,
    pub effective_ui: bool,
    pub content_hash: Option<String>,
}

impl ChangedComponent {
    pub fn new(kind: ChangeKind, path: impl Into<String>) -> Result<Self> {
        let path = normalize_path(path.into())?;
        Ok(Self {
            kind,
            generated: is_generated_path(&path),
            effective_ui: is_effective_ui_path(&path),
            path,
            previous_path: None,
            package_owner: None,
            content_hash: None,
        })
    }

    pub fn renamed(previous_path: impl Into<String>, path: impl Into<String>) -> Result<Self> {
        let mut component = Self::new(ChangeKind::Renamed, path)?;
        component.previous_path = Some(normalize_path(previous_path.into())?);
        Ok(component)
    }

    #[must_use]
    pub fn all_paths(&self) -> Vec<&str> {
        self.previous_path
            .as_deref()
            .into_iter()
            .chain(std::iter::once(self.path.as_str()))
            .collect()
    }
}

pub fn normalize_components(
    repo: &Path,
    components: &[ChangedComponent],
) -> Result<Vec<ChangedComponent>> {
    if components.is_empty() {
        return Err(EvidenceError::Validation(
            "changed-component scope cannot be empty".to_owned(),
        ));
    }
    let mut normalized = Vec::with_capacity(components.len());
    for source in components {
        let mut component = source.clone();
        component.path = normalize_path(component.path)?;
        component.previous_path = component.previous_path.map(normalize_path).transpose()?;
        component.generated = is_generated_path(&component.path);
        component.effective_ui = is_effective_ui_path(&component.path);
        component.package_owner = infer_package_owner(repo, &component.path);
        if component.kind != ChangeKind::Deleted {
            let absolute = repo.join(&component.path);
            if absolute.is_file() {
                component.content_hash = Some(hash_bytes(&fs::read(absolute)?));
            }
        }
        normalized.push(component);
    }
    normalized.sort();
    normalized.dedup();
    Ok(normalized)
}

pub fn changed_scope_fingerprint(components: &[ChangedComponent]) -> String {
    let mut scope = components.to_vec();
    for component in &mut scope {
        component.content_hash = None;
    }
    fingerprint(&scope)
}

pub fn is_generated_path(path: &str) -> bool {
    let path = path.replace('\\', "/").to_ascii_lowercase();
    path.starts_with("dist/")
        || path.starts_with("target/")
        || path.contains("/generated/")
        || path.contains("/dist/")
        || path.contains("/target/")
        || path.contains("__generated__")
        || path.ends_with(".min.js")
        || path.ends_with(".min.css")
}

pub fn is_effective_ui_path(path: &str) -> bool {
    let path = path.replace('\\', "/").to_ascii_lowercase();
    if path.starts_with("docs/")
        || path.contains("/docs/")
        || path.contains("__snapshots__")
        || path.ends_with(".snap")
        || path.ends_with(".md")
        || path.ends_with(".lock")
        || is_generated_path(&path)
    {
        return false;
    }
    path.starts_with("apps/")
        || path.starts_with("web/")
        || path.starts_with("frontend/")
        || path.starts_with("src-tauri/")
        || matches!(
            Path::new(&path)
                .extension()
                .and_then(|value| value.to_str()),
            Some("html" | "css" | "scss" | "tsx" | "jsx" | "vue" | "svelte")
        )
}

pub(crate) fn requires_artifact_semantics(component: &ChangedComponent) -> bool {
    component.generated
        || matches!(component.kind, ChangeKind::Added | ChangeKind::Renamed)
        || Path::new(&component.path)
            .extension()
            .and_then(|value| value.to_str())
            .is_some_and(|extension| {
                matches!(
                    extension.to_ascii_lowercase().as_str(),
                    "html" | "json" | "png" | "pdf" | "zip" | "jar" | "docx" | "xlsx" | "pptx"
                )
            })
}

pub(crate) fn requires_security(component: &ChangedComponent) -> bool {
    let path = component.path.to_ascii_lowercase();
    path.ends_with("cargo.lock")
        || path.ends_with("package-lock.json")
        || path.ends_with("pnpm-lock.yaml")
        || path.ends_with("yarn.lock")
        || path.contains("security")
        || path.contains("auth")
        || path.starts_with(".github/workflows/")
}

pub(crate) fn package_owners(components: &[ChangedComponent]) -> BTreeSet<&str> {
    components
        .iter()
        .map(|component| component.package_owner.as_deref().unwrap_or("."))
        .collect()
}

fn infer_package_owner(repo: &Path, path: &str) -> Option<String> {
    let mut current = Path::new(path).parent();
    while let Some(relative) = current {
        let root = repo.join(relative);
        if [
            "Cargo.toml",
            "package.json",
            "pyproject.toml",
            "go.mod",
            "pom.xml",
            "build.gradle",
            "build.gradle.kts",
            "CMakeLists.txt",
        ]
        .iter()
        .any(|manifest| root.join(manifest).is_file())
        {
            return Some(if relative.as_os_str().is_empty() {
                ".".to_owned()
            } else {
                relative.to_string_lossy().replace('\\', "/")
            });
        }
        current = relative.parent();
    }
    None
}

fn normalize_path(path: String) -> Result<String> {
    let path = path
        .replace('\\', "/")
        .trim_start_matches("./")
        .trim_end_matches('/')
        .to_owned();
    let parsed = Path::new(&path);
    if path.is_empty()
        || parsed.is_absolute()
        || parsed.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(EvidenceError::Validation(format!(
            "invalid repository-relative path: {path}"
        )));
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_rename_delete_and_ui_semantics() {
        let directory = tempfile::tempdir().unwrap();
        let repo = directory.path();
        fs::create_dir_all(repo.join("apps/web/src")).unwrap();
        fs::write(repo.join("apps/web/package.json"), "{}").unwrap();
        fs::write(repo.join("apps/web/src/New.tsx"), "export default 1").unwrap();
        let components = vec![
            ChangedComponent::renamed("apps/web/src/Old.tsx", "apps/web/src/New.tsx").unwrap(),
            ChangedComponent::new(ChangeKind::Deleted, "apps/web/src/gone.css").unwrap(),
        ];
        let normalized = normalize_components(repo, &components).unwrap();
        assert_eq!(normalized[0].package_owner.as_deref(), Some("apps/web"));
        assert!(normalized.iter().all(|component| component.effective_ui));
        assert!(
            normalized
                .iter()
                .any(|component| component.previous_path.is_some())
        );
        assert!(
            normalized
                .iter()
                .any(|component| component.kind == ChangeKind::Deleted)
        );
    }
}
