//! Deterministic, conflict-safe Markdown memory writeback planning.

use std::collections::{BTreeMap, BTreeSet};
use medusa_memory_consolidation::{ConsolidatedMemory, MemoryConflict, MemoryKind};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const GENERATED_START: &str = "<!-- medusa:generated:start -->";
pub const GENERATED_END: &str = "<!-- medusa:generated:end -->";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct MemoryDocument {
    pub path: String,
    pub content: String,
    pub expected_fingerprint: Option<String>,
}

impl MemoryDocument {
    pub fn validate(&self) -> Result<(), &'static str> {
        validate_path(&self.path)?;
        if self.content.matches(GENERATED_START).count() > 1 || self.content.matches(GENERATED_END).count() > 1 {
            return Err("memory document contains duplicate generated markers");
        }
        if self.content.contains(GENERATED_START) != self.content.contains(GENERATED_END) {
            return Err("memory document generated markers are unbalanced");
        }
        if self.expected_fingerprint.as_ref().is_some_and(|expected| expected != &fingerprint_bytes(self.content.as_bytes())) {
            return Err("memory document changed since it was read");
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct WritebackPolicy {
    pub default_path: String,
    #[serde(default)]
    pub paths_by_kind: BTreeMap<MemoryKind, String>,
    pub include_provenance: bool,
    pub include_conflicts: bool,
}

impl Default for WritebackPolicy {
    fn default() -> Self {
        Self { default_path: "memory/MEMORY.md".into(), paths_by_kind: BTreeMap::new(), include_provenance: true, include_conflicts: true }
    }
}

impl WritebackPolicy {
    pub fn validate(&self) -> Result<(), &'static str> {
        validate_path(&self.default_path)?;
        for path in self.paths_by_kind.values() { validate_path(path)?; }
        Ok(())
    }
    fn path_for(&self, kind: MemoryKind) -> &str {
        self.paths_by_kind.get(&kind).map_or(self.default_path.as_str(), String::as_str)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WriteOperation { Create, ReplaceGeneratedRegion, NoChange }

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DocumentPatch {
    pub path: String,
    pub operation: WriteOperation,
    pub before_fingerprint: Option<String>,
    pub after_fingerprint: String,
    pub content: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct WritebackPlan {
    pub patches: Vec<DocumentPatch>,
    pub unresolved_conflict_fingerprints: Vec<String>,
    pub source_fingerprint: String,
    pub plan_fingerprint: String,
}

pub fn plan_writeback(memories: &[ConsolidatedMemory], conflicts: &[MemoryConflict], documents: &[MemoryDocument], policy: &WritebackPolicy) -> Result<WritebackPlan, &'static str> {
    policy.validate()?;
    let mut canonical_documents = documents.to_vec();
    canonical_documents.sort_by(|a, b| a.path.cmp(&b.path));
    let mut paths = BTreeSet::new();
    let mut current = BTreeMap::new();
    for document in &canonical_documents {
        document.validate()?;
        if !paths.insert(document.path.as_str()) { return Err("memory document paths must be unique"); }
        current.insert(document.path.clone(), document.clone());
    }

    let mut canonical_memories = memories.to_vec();
    canonical_memories.sort_by(|a, b| a.kind.cmp(&b.kind).then_with(|| a.subject.cmp(&b.subject)).then_with(|| a.predicate.cmp(&b.predicate)).then_with(|| a.key.cmp(&b.key)));
    let mut keys = BTreeSet::new();
    for memory in &canonical_memories {
        if memory.key.trim().is_empty() || memory.value.trim().is_empty() { return Err("consolidated memory fields cannot be empty"); }
        if !keys.insert(memory.key.as_str()) { return Err("consolidated memory keys must be unique"); }
    }

    let mut canonical_conflicts = conflicts.to_vec();
    canonical_conflicts.sort_by(|a, b| a.key.cmp(&b.key).then_with(|| a.fingerprint.cmp(&b.fingerprint)));
    let mut conflict_fingerprints = BTreeSet::new();
    for conflict in &canonical_conflicts {
        if conflict.key.trim().is_empty() || conflict.fingerprint.trim().is_empty() { return Err("memory conflict fields cannot be empty"); }
        if !conflict_fingerprints.insert(conflict.fingerprint.as_str()) { return Err("memory conflict fingerprints must be unique"); }
    }

    let mut grouped: BTreeMap<String, Vec<&ConsolidatedMemory>> = BTreeMap::new();
    for memory in &canonical_memories { grouped.entry(policy.path_for(memory.kind).to_owned()).or_default().push(memory); }
    if policy.include_conflicts && !canonical_conflicts.is_empty() { grouped.entry(policy.default_path.clone()).or_default(); }

    let mut patches = Vec::new();
    for (path, path_memories) in grouped {
        validate_path(&path)?;
        let path_conflicts: &[MemoryConflict] = if policy.include_conflicts && path == policy.default_path { &canonical_conflicts } else { &[] };
        let generated = render_generated(&path_memories, path_conflicts, policy.include_provenance);
        let existing = current.get(&path);
        let content = match existing { Some(document) => replace_generated_region(&document.content, &generated)?, None => format!("# Medusa Memory\n\n{generated}") };
        let before_fingerprint = existing.map(|document| fingerprint_bytes(document.content.as_bytes()));
        let after_fingerprint = fingerprint_bytes(content.as_bytes());
        let operation = match &before_fingerprint { None => WriteOperation::Create, Some(before) if before == &after_fingerprint => WriteOperation::NoChange, Some(_) => WriteOperation::ReplaceGeneratedRegion };
        patches.push(DocumentPatch { path, operation, before_fingerprint, after_fingerprint, content });
    }

    let unresolved_conflict_fingerprints = canonical_conflicts.iter().map(|item| item.fingerprint.clone()).collect::<Vec<_>>();
    let source_fingerprint = fingerprint(&(&canonical_memories, &canonical_conflicts, &canonical_documents, policy));
    let plan_fingerprint = fingerprint(&(&patches, &unresolved_conflict_fingerprints, &source_fingerprint));
    Ok(WritebackPlan { patches, unresolved_conflict_fingerprints, source_fingerprint, plan_fingerprint })
}

fn render_generated(memories: &[&ConsolidatedMemory], conflicts: &[MemoryConflict], include_provenance: bool) -> String {
    let mut output = format!("{GENERATED_START}\n\n");
    let mut current_kind = None;
    for memory in memories {
        if current_kind != Some(memory.kind) {
            current_kind = Some(memory.kind);
            output.push_str(&format!("## {}\n\n", kind_heading(memory.kind)));
        }
        output.push_str(&format!("- **{} — {}:** {}", memory.subject.trim(), memory.predicate.trim(), memory.value.trim()));
        if include_provenance {
            output.push_str(&format!(" <!-- support:{} confidence:{} fingerprint:{} -->", memory.support_ids.join(","), memory.confidence_basis_points, memory.fingerprint));
        }
        output.push('\n');
    }
    if !conflicts.is_empty() {
        output.push_str("\n## Unresolved conflicts\n\n");
        for conflict in conflicts {
            output.push_str(&format!("- **{}:** {} <!-- conflict:{} -->\n", conflict.key, conflict.candidate_values.join(" | "), conflict.fingerprint));
        }
    }
    output.push_str(&format!("\n{GENERATED_END}\n"));
    output
}

fn replace_generated_region(existing: &str, generated: &str) -> Result<String, &'static str> {
    match (existing.find(GENERATED_START), existing.find(GENERATED_END)) {
        (None, None) => Ok(format!("{}{}{}", existing.trim_end(), if existing.trim().is_empty() { "" } else { "\n\n" }, generated)),
        (Some(start), Some(end)) if start < end => {
            let suffix = end + GENERATED_END.len();
            Ok(format!("{}{}{}", &existing[..start], generated.trim_end(), &existing[suffix..]))
        }
        _ => Err("memory document generated markers are malformed"),
    }
}

fn validate_path(path: &str) -> Result<(), &'static str> {
    if path.trim().is_empty() || !path.ends_with(".md") { return Err("memory writeback paths must be non-empty Markdown paths"); }
    if path.starts_with('/') || path.split('/').any(|segment| segment == "..") { return Err("memory writeback paths must remain workspace-relative"); }
    Ok(())
}

fn kind_heading(kind: MemoryKind) -> &'static str {
    match kind { MemoryKind::Preference => "Preferences", MemoryKind::Decision => "Decisions", MemoryKind::Constraint => "Constraints", MemoryKind::Fact => "Facts", MemoryKind::Procedure => "Procedures", MemoryKind::FailureLesson => "Failure lessons" }
}

fn fingerprint<T: Serialize>(value: &T) -> String { fingerprint_bytes(&serde_json::to_vec(value).expect("in-memory writeback serialization cannot fail")) }
fn fingerprint_bytes(bytes: &[u8]) -> String { hex::encode(Sha256::digest(bytes)) }

#[cfg(test)]
mod tests {
    use super::*;
    fn memory(value: &str) -> ConsolidatedMemory {
        ConsolidatedMemory { key: "project\u{1f}language\u{1f}Fact".into(), subject: "Project".into(), predicate: "language".into(), value: value.into(), kind: MemoryKind::Fact, support_ids: vec!["obs-1".into(), "obs-2".into()], confidence_basis_points: 9000, fingerprint: "memory-fingerprint".into() }
    }
    #[test]
    fn preserves_manual_content_around_generated_region() {
        let document = MemoryDocument { path: "memory/MEMORY.md".into(), content: format!("# Notes\n\nManual before.\n\n{GENERATED_START}\nold\n{GENERATED_END}\n\nManual after.\n"), expected_fingerprint: None };
        let plan = plan_writeback(&[memory("Rust")], &[], &[document], &WritebackPolicy::default()).unwrap();
        assert!(plan.patches[0].content.contains("Manual before."));
        assert!(plan.patches[0].content.contains("Manual after."));
        assert!(!plan.patches[0].content.contains("\nold\n"));
    }
    #[test]
    fn identical_render_is_no_change() {
        let first = plan_writeback(&[memory("Rust")], &[], &[], &WritebackPolicy::default()).unwrap();
        let document = MemoryDocument { path: first.patches[0].path.clone(), content: first.patches[0].content.clone(), expected_fingerprint: Some(first.patches[0].after_fingerprint.clone()) };
        let second = plan_writeback(&[memory("Rust")], &[], &[document], &WritebackPolicy::default()).unwrap();
        assert_eq!(second.patches[0].operation, WriteOperation::NoChange);
    }
    #[test]
    fn rejects_stale_snapshot() {
        let document = MemoryDocument { path: "memory/MEMORY.md".into(), content: "changed".into(), expected_fingerprint: Some("stale".into()) };
        assert!(plan_writeback(&[memory("Rust")], &[], &[document], &WritebackPolicy::default()).is_err());
    }
    #[test]
    fn input_order_does_not_change_plan() {
        let mut second = memory("Cargo");
        second.key = "project\u{1f}build\u{1f}Fact".into();
        second.predicate = "build".into();
        let left = plan_writeback(&[memory("Rust"), second.clone()], &[], &[], &WritebackPolicy::default()).unwrap();
        let right = plan_writeback(&[second, memory("Rust")], &[], &[], &WritebackPolicy::default()).unwrap();
        assert_eq!(left, right);
    }
}
