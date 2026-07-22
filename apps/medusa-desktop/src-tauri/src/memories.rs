use std::{fs, path::{Path, PathBuf}};

use medusa_memory::{MemoryDocument, Scope, Status, Validation};
use serde::Serialize;

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopMemory {
    id: String,
    memory_type: String,
    title: String,
    body: String,
    created_at: String,
    updated_at: String,
    scope: String,
    project_id: Option<String>,
    session_id: Option<String>,
    status: String,
    confidence_milli: u16,
    validation: String,
    sources: Vec<String>,
    supersedes: Vec<String>,
    superseded_by: Vec<String>,
    tags: Vec<String>,
    expires_at: Option<String>,
    last_validated_at: String,
    successful_reuse_count: u32,
    path: String,
}

#[tauri::command]
pub fn runtime_list_memories(
    repo: String,
    query: Option<String>,
    include_inactive: Option<bool>,
) -> Result<Vec<DesktopMemory>, String> {
    let repo = canonical_repo(&repo)?;
    let root = repo.join(".medusa/memory");
    if !root.exists() {
        return Ok(Vec::new());
    }
    let query = query.unwrap_or_default().to_lowercase();
    let include_inactive = include_inactive.unwrap_or(false);
    let mut entries = Vec::new();
    collect_markdown(&root, &root, &query, include_inactive, &mut entries)?;
    entries.sort_by(|left, right| right.updated_at.cmp(&left.updated_at).then_with(|| left.title.cmp(&right.title)));
    Ok(entries)
}

fn canonical_repo(repo: &str) -> Result<PathBuf, String> {
    if repo.trim().is_empty() {
        return Err("A repository is required to browse memory.".into());
    }
    fs::canonicalize(repo).map_err(|error| format!("Could not open repository: {error}"))
}

fn collect_markdown(
    directory: &Path,
    root: &Path,
    query: &str,
    include_inactive: bool,
    output: &mut Vec<DesktopMemory>,
) -> Result<(), String> {
    for entry in fs::read_dir(directory).map_err(|error| format!("Could not read memory directory: {error}"))? {
        let entry = entry.map_err(|error| format!("Could not read memory entry: {error}"))?;
        let path = entry.path();
        if path.is_dir() {
            if path.file_name().is_some_and(|name| name == "proposals") {
                continue;
            }
            collect_markdown(&path, root, query, include_inactive, output)?;
            continue;
        }
        if path.extension().and_then(|value| value.to_str()) != Some("md") {
            continue;
        }
        let text = fs::read_to_string(&path).map_err(|error| format!("Could not read {}: {error}", path.display()))?;
        let document = MemoryDocument::from_markdown(&text)
            .map_err(|error| format!("Invalid canonical memory {}: {error}", path.display()))?;
        if !include_inactive && document.status != Status::Active {
            continue;
        }
        if !query.is_empty() && !matches_query(&document, query) {
            continue;
        }
        output.push(to_desktop(document, root, &path));
    }
    Ok(())
}

fn matches_query(document: &MemoryDocument, query: &str) -> bool {
    document.title.to_lowercase().contains(query)
        || document.body.to_lowercase().contains(query)
        || document.memory_type.to_lowercase().contains(query)
        || document.tags.iter().any(|tag| tag.to_lowercase().contains(query))
        || document.sources.iter().any(|source| source.to_lowercase().contains(query))
}

fn to_desktop(document: MemoryDocument, root: &Path, path: &Path) -> DesktopMemory {
    DesktopMemory {
        id: document.id,
        memory_type: document.memory_type,
        title: document.title,
        body: document.body,
        created_at: document.created_at,
        updated_at: document.updated_at,
        scope: scope_name(document.scope).into(),
        project_id: document.project_id,
        session_id: document.session_id,
        status: status_name(document.status).into(),
        confidence_milli: document.confidence_milli,
        validation: validation_name(document.validation).into(),
        sources: document.sources,
        supersedes: document.supersedes,
        superseded_by: document.superseded_by,
        tags: document.tags,
        expires_at: document.expires_at,
        last_validated_at: document.last_validated_at,
        successful_reuse_count: document.successful_reuse_count,
        path: path.strip_prefix(root).unwrap_or(path).display().to_string(),
    }
}

fn scope_name(value: Scope) -> &'static str {
    match value { Scope::Project => "project", Scope::User => "user" }
}

fn status_name(value: Status) -> &'static str {
    match value { Status::Active => "active", Status::Superseded => "superseded", Status::Archived => "archived" }
}

fn validation_name(value: Validation) -> &'static str {
    match value {
        Validation::UserStated => "user-stated",
        Validation::Observed => "observed",
        Validation::TestVerified => "test-verified",
        Validation::SourceVerified => "source-verified",
        Validation::Inferred => "inferred",
        Validation::Unverified => "unverified",
        Validation::Contradicted => "contradicted",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn memory(title: &str, status: &str) -> String {
        format!("---\nid: mem-1\ntype: decision\ntitle: {title}\ncreated_at: 2026-07-22T00:00:00Z\nupdated_at: 2026-07-22T01:00:00Z\nscope: project\nproject_id: sha256:test\nsession_id: ses-1\nstatus: {status}\nconfidence_milli: 940\nvalidation: test-verified\nsources: artifact://sessions/ses-1/test\nsupersedes: \nsuperseded_by: \ntags: rust, desktop\nexpires_at: \nlast_validated_at: 2026-07-22T01:00:00Z\nsuccessful_reuse_count: 2\n---\n\nUse the verified path.\n")
    }

    #[test]
    fn lists_active_memory_with_provenance() {
        let directory = crate::tempdir().expect("tempdir");
        let lessons = directory.path().join(".medusa/memory/lessons");
        fs::create_dir_all(&lessons).expect("create lessons");
        fs::write(lessons.join("memory.md"), memory("Verified decision", "active")).expect("write memory");
        let items = runtime_list_memories(directory.path().display().to_string(), None, None).expect("list");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].validation, "test-verified");
        assert_eq!(items[0].sources.len(), 1);
        assert_eq!(items[0].successful_reuse_count, 2);
    }

    #[test]
    fn filters_inactive_and_searches_content() {
        let directory = crate::tempdir().expect("tempdir");
        let lessons = directory.path().join(".medusa/memory/lessons");
        fs::create_dir_all(&lessons).expect("create lessons");
        fs::write(lessons.join("active.md"), memory("Cargo workflow", "active")).expect("write active");
        fs::write(lessons.join("old.md"), memory("Old workflow", "archived")).expect("write archived");
        let active = runtime_list_memories(directory.path().display().to_string(), Some("cargo".into()), None).expect("search");
        assert_eq!(active.len(), 1);
        let all = runtime_list_memories(directory.path().display().to_string(), None, Some(true)).expect("all");
        assert_eq!(all.len(), 2);
    }
}
