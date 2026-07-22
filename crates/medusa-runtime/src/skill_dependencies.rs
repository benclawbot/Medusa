use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Component, Path, PathBuf},
};

use serde::Serialize;
use serde_json::Value;

const MANIFEST: &str = "dependencies.json";
const SKILL_FILE: &str = "SKILL.md";

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ResolvedSkillGraph {
    pub selected: String,
    pub order: Vec<String>,
    pub direct: Vec<String>,
    pub content: String,
    pub total_bytes: usize,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct DependencyInspection {
    pub skill: String,
    pub direct: Vec<String>,
    pub transitive_order: Vec<String>,
    pub reverse_dependents: Vec<String>,
}

pub fn resolve_project_skill(
    root: &Path,
    selected: &str,
    max_bytes: usize,
) -> Result<ResolvedSkillGraph, String> {
    validate_name(selected)?;
    if max_bytes != usize::MAX {
        crate::skill_dependency_locks::verify_dependency_lock_if_present(root, selected)?;
    }
    resolve_project_skill_unverified(root, selected, max_bytes)
}

fn resolve_project_skill_unverified(
    root: &Path,
    selected: &str,
    max_bytes: usize,
) -> Result<ResolvedSkillGraph, String> {
    let graph = load_graph(root)?;
    let direct = graph
        .get(selected)
        .cloned()
        .ok_or_else(|| format!("approved project skill `{selected}` was not found"))?;
    let order = topological_order(&graph, selected)?;
    let mut total_bytes = 0usize;
    let mut sections = Vec::with_capacity(order.len());
    for name in &order {
        let path = confined_skill_path(root, name)?;
        let bytes = fs::read(&path).map_err(|error| format!("read {}: {error}", path.display()))?;
        total_bytes = total_bytes
            .checked_add(bytes.len())
            .ok_or_else(|| "resolved skill dependency graph exceeds addressable size".to_owned())?;
        if total_bytes > max_bytes {
            return Err(format!(
                "resolved skill dependency graph for `{selected}` is {total_bytes} bytes; limit is {max_bytes}"
            ));
        }
        let content = String::from_utf8(bytes)
            .map_err(|_| format!("skill file is not UTF-8: {}", path.display()))?;
        sections.push(format!("--- approved project skill: {name} ---\n{content}\n--- end approved project skill: {name} ---"));
    }
    Ok(ResolvedSkillGraph {
        selected: selected.to_owned(),
        order,
        direct,
        content: sections.join("\n\n"),
        total_bytes,
    })
}

pub fn inspect_project_skill(root: &Path, selected: &str) -> Result<DependencyInspection, String> {
    validate_name(selected)?;
    let graph = load_graph(root)?;
    let direct = graph
        .get(selected)
        .cloned()
        .ok_or_else(|| format!("approved project skill `{selected}` was not found"))?;
    Ok(DependencyInspection {
        skill: selected.to_owned(),
        direct,
        transitive_order: topological_order(&graph, selected)?,
        reverse_dependents: reverse_dependents_in_graph(&graph, selected)?,
    })
}

pub fn validate_project_graph(root: &Path) -> Result<Vec<String>, String> {
    let graph = load_graph(root)?;
    for name in graph.keys() {
        let _ = topological_order(&graph, name)?;
    }
    Ok(graph.keys().cloned().collect())
}

pub fn reverse_dependents(root: &Path, target: &str) -> Result<Vec<String>, String> {
    validate_name(target)?;
    reverse_dependents_in_graph(&load_graph(root)?, target)
}

fn load_graph(root: &Path) -> Result<BTreeMap<String, Vec<String>>, String> {
    let canonical_root = fs::canonicalize(root)
        .map_err(|error| format!("resolve approved skill root {}: {error}", root.display()))?;
    let entries = fs::read_dir(&canonical_root).map_err(|error| {
        format!(
            "read approved skill root {}: {error}",
            canonical_root.display()
        )
    })?;
    let mut graph = BTreeMap::new();
    for entry in entries {
        let entry = entry.map_err(|error| format!("read approved skill entry: {error}"))?;
        let file_type = entry
            .file_type()
            .map_err(|error| format!("inspect {}: {error}", entry.path().display()))?;
        if file_type.is_symlink() {
            return Err(format!(
                "approved skill entry escapes approved skill root through symlink: {}",
                entry.path().display()
            ));
        }
        if !file_type.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        validate_name(&name)?;
        if !confined_skill_path(&canonical_root, &name)?.is_file() {
            continue;
        }
        graph.insert(name.clone(), read_dependencies(&canonical_root, &name)?);
    }
    for (skill, dependencies) in &graph {
        for dependency in dependencies {
            if dependency == skill {
                return Err(format!("skill `{skill}` cannot depend on itself"));
            }
            if !graph.contains_key(dependency) {
                return Err(format!(
                    "skill `{skill}` requires missing approved project skill `{dependency}`"
                ));
            }
        }
    }
    Ok(graph)
}

fn read_dependencies(root: &Path, name: &str) -> Result<Vec<String>, String> {
    let manifest = confined_directory(root, name)?.join(MANIFEST);
    if !manifest.exists() {
        return Ok(Vec::new());
    }
    let canonical = fs::canonicalize(&manifest)
        .map_err(|error| format!("resolve {}: {error}", manifest.display()))?;
    if !canonical.starts_with(root) {
        return Err(format!(
            "dependency manifest for `{name}` escapes approved skill root"
        ));
    }
    let value: Value = serde_json::from_slice(
        &fs::read(&canonical).map_err(|error| format!("read {}: {error}", canonical.display()))?,
    )
    .map_err(|error| format!("parse {}: {error}", canonical.display()))?;
    let object = value
        .as_object()
        .ok_or_else(|| format!("{} must contain a JSON object", canonical.display()))?;
    if object.get("schema_version").and_then(Value::as_u64) != Some(1) {
        return Err(format!("{} requires schema_version 1", canonical.display()));
    }
    let requires = match object.get("requires") {
        None => return Ok(Vec::new()),
        Some(value) => value
            .as_array()
            .ok_or_else(|| format!("{}.requires must be an array", canonical.display()))?,
    };
    let mut dependencies = Vec::with_capacity(requires.len());
    let mut seen = BTreeSet::new();
    for value in requires {
        let dependency = value
            .as_str()
            .ok_or_else(|| format!("{}.requires entries must be strings", canonical.display()))?;
        validate_name(dependency)?;
        if !seen.insert(dependency.to_owned()) {
            return Err(format!(
                "skill `{name}` declares duplicate dependency `{dependency}`"
            ));
        }
        dependencies.push(dependency.to_owned());
    }
    dependencies.sort();
    Ok(dependencies)
}

fn topological_order(
    graph: &BTreeMap<String, Vec<String>>,
    selected: &str,
) -> Result<Vec<String>, String> {
    let mut state = BTreeMap::new();
    let mut stack = Vec::new();
    let mut order = Vec::new();
    visit(selected, graph, &mut state, &mut stack, &mut order)?;
    Ok(order)
}

fn visit(
    name: &str,
    graph: &BTreeMap<String, Vec<String>>,
    state: &mut BTreeMap<String, VisitState>,
    stack: &mut Vec<String>,
    order: &mut Vec<String>,
) -> Result<(), String> {
    match state.get(name) {
        Some(VisitState::Complete) => return Ok(()),
        Some(VisitState::Visiting) => {
            let start = stack.iter().position(|item| item == name).unwrap_or(0);
            let mut cycle = stack[start..].to_vec();
            cycle.push(name.to_owned());
            return Err(format!("skill dependency cycle: {}", cycle.join(" -> ")));
        }
        None => {}
    }
    let dependencies = graph
        .get(name)
        .ok_or_else(|| format!("approved project skill `{name}` was not found"))?;
    state.insert(name.to_owned(), VisitState::Visiting);
    stack.push(name.to_owned());
    for dependency in dependencies {
        visit(dependency, graph, state, stack, order)?;
    }
    stack.pop();
    state.insert(name.to_owned(), VisitState::Complete);
    order.push(name.to_owned());
    Ok(())
}

fn reverse_dependents_in_graph(
    graph: &BTreeMap<String, Vec<String>>,
    target: &str,
) -> Result<Vec<String>, String> {
    if !graph.contains_key(target) {
        return Err(format!("approved project skill `{target}` was not found"));
    }
    let mut dependents = Vec::new();
    for name in graph.keys() {
        if name != target
            && topological_order(graph, name)?
                .iter()
                .any(|dependency| dependency == target)
        {
            dependents.push(name.clone());
        }
    }
    Ok(dependents)
}

fn confined_skill_path(root: &Path, name: &str) -> Result<PathBuf, String> {
    Ok(confined_directory(root, name)?.join(SKILL_FILE))
}
fn confined_directory(root: &Path, name: &str) -> Result<PathBuf, String> {
    validate_name(name)?;
    let directory = root.join(name);
    let canonical = fs::canonicalize(&directory)
        .map_err(|error| format!("resolve {}: {error}", directory.display()))?;
    if !canonical.starts_with(root) {
        return Err(format!("skill `{name}` escapes approved skill root"));
    }
    Ok(canonical)
}
fn validate_name(name: &str) -> Result<(), String> {
    let path = Path::new(name);
    if name.is_empty()
        || name == "."
        || name == ".."
        || name.contains(['/', '\\', '@'])
        || name.contains("..")
        || path.components().count() != 1
        || !matches!(path.components().next(), Some(Component::Normal(_)))
    {
        return Err(format!("invalid skill name `{name}`"));
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum VisitState {
    Visiting,
    Complete,
}

pub fn validate_restorable_skill(
    active_root: &Path,
    candidate: &Path,
    name: &str,
) -> Result<(), String> {
    validate_name(name)?;
    let graph = load_graph(active_root)?;
    let manifest = candidate.join(MANIFEST);
    if !manifest.exists() {
        return Ok(());
    }
    let canonical_candidate = fs::canonicalize(candidate)
        .map_err(|error| format!("resolve {}: {error}", candidate.display()))?;
    let canonical_manifest = fs::canonicalize(&manifest)
        .map_err(|error| format!("resolve {}: {error}", manifest.display()))?;
    if !canonical_manifest.starts_with(&canonical_candidate) {
        return Err(format!(
            "dependency manifest for `{name}` escapes quarantined skill directory"
        ));
    }
    let value: Value = serde_json::from_slice(
        &fs::read(&canonical_manifest)
            .map_err(|error| format!("read {}: {error}", canonical_manifest.display()))?,
    )
    .map_err(|error| format!("parse {}: {error}", canonical_manifest.display()))?;
    if value.get("schema_version").and_then(Value::as_u64) != Some(1) {
        return Err(format!(
            "{} requires schema_version 1",
            canonical_manifest.display()
        ));
    }
    let requires = value.get("requires").map_or(Ok(&[][..]), |value| {
        value
            .as_array()
            .map(Vec::as_slice)
            .ok_or_else(|| format!("{}.requires must be an array", canonical_manifest.display()))
    })?;
    let mut seen = BTreeSet::new();
    for value in requires {
        let dependency = value.as_str().ok_or_else(|| {
            format!(
                "{}.requires entries must be strings",
                canonical_manifest.display()
            )
        })?;
        validate_name(dependency)?;
        if dependency == name {
            return Err(format!("skill `{name}` cannot depend on itself"));
        }
        if !seen.insert(dependency) {
            return Err(format!(
                "skill `{name}` declares duplicate dependency `{dependency}`"
            ));
        }
        if !graph.contains_key(dependency) {
            return Err(format!(
                "skill `{name}` requires unavailable approved project skill `{dependency}`"
            ));
        }
    }
    Ok(())
}
