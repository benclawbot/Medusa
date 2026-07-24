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
        if self.path.trim().is_empty() {
            return Err("memory document path cannot be empty");
        }
        if !self.path.ends_with(".md") {
            return Err("memory document path must end in .md");
        }
        if self.content.matches(GENERATED_START).count() > 1
            || self.content.matches(GENERATED_END).count() > 1
        {
            return Err("memory document contains duplicate generated markers");
        }
        if self.content.contains(GENERATED_START) != self.content.contains(GENERATED_END) {
            return Err("memory document generated markers are unbalanced");
        }
        if let Some(expected) = &self.expected_fingerprint {
            if expected != &fingerprint_bytes(self.content.as_bytes()) {
                return Err("memory document changed since it was read");
            }
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
        Self {
            default_path: "memory/MEMORY.md".into(),
            paths_by_kind: BTreeMap::new(),
            include_provenance: true,
            include_conflicts: true,
        }
    }
}

impl WritebackPolicy {
    pub fn validate(&self) -> Result<(), &'static str> {
        validate_path(&self.default_path)?;
        for path in self.paths_by_kind.values() {
            validate_path(path)?;
        }
        Ok(())
    }

    fn path_for(&self, kind: MemoryKind) -> &str {
        self.paths_by_kind
            .get(&kind)
            .map_or(self.default_path.as_str(), String::as_str)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WriteOperation {
    Create,
    ReplaceGeneratedRegion,
    NoChange,
}

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

pub fn plan_writeback(
    memories: &[ConsolidatedMemory],
    conflicts: &[MemoryConflict],
    documents: &[MemoryDocument],
    policy: &WritebackPolicy,
) -> Result<WritebackPlan, &'static str> {
    policy.validate()?;

    let mut document_paths = BTreeSet::new();
    let mut current = BTreeMap::new();
    for document in documents {
        document.validate()?;
        if !document_paths.insert(document.path.as_str()) {
            return Err("memory document paths must be unique");
        }
        current.insert(document.path.clone(), document.clone());
    }

    let mut memory_keys = BTreeSet::new();
    for memory in memories {
        if memory.key.trim().is_empty() || memory.value.trim().is_empty() {
            return Err("consolidated memory fields cannot be empty");
        }
        if !memory_keys.insert(memory.key.as_str()) {
            return Err("consolidated memory keys must be unique");
        }
    }

    let mut grouped: BTreeMap<String, Vec<&ConsolidatedMemory>> = BTreeMap::new();
    for memory in memories {
        grouped
            .entry(policy.path_for(memory.kind).to_owned())
            .or_default()
            .push(memory);
    }

    let conflict_path = policy.default_path.clone();
    if policy.include_conflicts && !conflicts.is_empty() {
        grouped.entry(conflict_path).or_default();
    }

    let mut patches = Vec::new();
    for (path, mut path_memories) in grouped {
        validate_path(&path)?;
        path_memories.sort_by(|left, right| {
            left.kind
                .cmp(&right.kind)
                .then_with(|| left.subject.cmp(&right.subject))
                .then_with(|| left.predicate.cmp(&right.predicate))
                .then_with(|| left.key.cmp(&right.key))
        });

        let path_conflicts = if policy.include_conflicts && path == policy.default_path {
            conflicts
        } else {
            &[]
        };
        let generated = render_generated(&path_memories, path_conflicts, policy.include_provenance);
        let existing = current.get(&path);
        let next_content = match existing {
            Some(document) => replace_generated_region(&document.content, &generated)?,
            None => format!("# Medusa Memory\n\n{generated}"),
        };
        let before_fingerprint = existing.map(|document| fingerprint_bytes(document.content.as_bytes()));
        let after_fingerprint = fingerprint_bytes(next_content.as_bytes());
        let operation = match &before_fingerprint {
            None => WriteOperation::Create,
            Some(before) if before == &after_fingerprint => WriteOperation::NoChange,
            Some(_) => WriteOperation::ReplaceGeneratedRegion,
        };
        patches.push(DocumentPatch {
            path,
            operation,
            before_fingerprint,
            after_fingerprint,
            content: next_content,
        });
    }

    patches.sort_by(|left, right| left.path.cmp(&right.path));
    let mut unresolved = conflicts
        .iter()
        .map(|conflict| conflict.fingerprint.clone())
        .collect::<Vec<_>>();
    unresolved.sort();
    unresolved.dedup();

    let source_fingerprint = fingerprint(&(memories, conflicts, documents, policy));
    let plan_fingerprint = fingerprint(&(&patches, &unresolved, &source_fingerprint));
    Ok(WritebackPlan {
        patches,
        unresolved_conflict_fingerprints: unresolved,
        source_fingerprint,
        plan_fingerprint,
    })
}

fn render_generated(
    memories: &[&ConsolidatedMemory],
    conflicts: &[MemoryConflict],
    include_provenance: bool,
) -> String {
    let mut output = String::from(GENERATED_START);
    output.push_str("\n\n");
    let mut current_kind = None;
    for memory in memories {
        if current_kind != Some(memory.kind) {
            current_kind = Some(memory.kind);
            output.push_str("## ");
            output.push_str(kind_heading(memory.kind));
            output.push_str("\n\n");
        }
        output.push_str("- **");
        output.push_str(memory.subject.trim());
        output.push_str(" — ");
        output.push_str(memory.predicate.trim());
        output.push_str(":** ");
        output.push_str(memory.value.trim());
        if include_provenance {
            output.push_str(" <!-- support:");
            output.push_str(&memory.support_ids.join(","));
            output.push_str(" confidence:");
            output.push_str(&memory.confidence_basis_points.to_string());
            output.push_str(" fingerprint:");
            output.push_str(&memory.fingerprint);
            output.push_str(" -->");
        }
        output.push('\n');
    }

    if !conflicts.is_empty() {
        output.push_str("\n## Unresolved conflicts\n\n");
        let mut ordered = conflicts.to_vec();
        ordered.sort_by(|left, right| left.key.cmp(&right.key));
        for conflict in ordered {
            output.push_str("- **");
            output.push_str(&conflict.key);
            output.push_str(":** ");
            output.push_str(&conflict.candidate_values.join(" | "));
            output.push_str(" <!-- conflict:");
            output.push_str(&conflict.fingerprint);
            output.push_str(" -->\n");
        }
    }

    output.push('\n');
    output.push_str(GENERATED_END);
    output.push('\n');
    output
}

fn replace_generated_region(existing: &str, generated: &str) -> Result<String, &'static str> {
    match (existing.find(GENERATED_START), existing.find(GENERATED_END)) {
        (None, None) => {
            let mut output = existing.trim_end().to_owned();
            if !output.is_empty() {
                output.push_str("\n\n");
            }
            output.push_str(generated);
            Ok(output)
        }
        (Some(start), Some(end)) if start < end => {
            let end = end + GENERATED_END.len();
            let mut output = String::with_capacity(existing.len() + generated.len());
            output.push_str(&existing[..start]);
            output.push_str(generated.trim_end());
            output.push_str(&existing[end..]);
            Ok(output)
        }
        _ => Err("memory document generated markers are malformed"),
    }
}

fn validate_path(path: &str) -> Result<(), &'static str> {
    if path.trim().is_empty() || !path.ends_with(".md") {
        return Err("memory writeback paths must be non-empty Markdown paths");
    }
    if path.starts_with('/') || path.split('/').any(|segment| segment == "..") {
        return Err("memory writeback paths must remain workspace-relative");
    }
    Ok(())
}

fn kind_heading(kind: MemoryKind) -> &'static str {
    match kind {
        MemoryKind::Preference => "Preferences",
        MemoryKind::Decision => "Decisions",
        MemoryKind::Constraint => "Constraints",
        MemoryKind::Fact => "Facts",
        MemoryKind::Procedure => "Procedures",
        MemoryKind::FailureLesson => "Failure lessons",
    }
}

fn fingerprint<T: Serialize>(value: &T) -> String {
    let bytes = serde_json::to_vec(value).expect("serializing in-memory writeback data cannot fail");
    fingerprint_bytes(&bytes)
}

fn fingerprint_bytes(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn memory(value: &str) -> ConsolidatedMemory {
        ConsolidatedMemory {
            key: "project\u{1f}language\u{1f}Fact".into(),
            subject: "Project".into(),
            predicate: "language".into(),
            value: value.into(),
            kind: MemoryKind::Fact,
            support_ids: vec!["obs-1".into(), "obs-2".into()],
            confidence_basis_points: 9000,
            fingerprint: "memory-fingerprint".into(),
        }
    }

    #[test]
    fn preserves_manual_content_around_generated_region() {
        let document = MemoryDocument {
            path: "memory/MEMORY.md".into(),
            content: format!("# Notes\n\nManual before.\n\n{GENERATED_START}\nold\n{GENERATED_END}\n\nManual after.\n"),
            expected_fingerprint: None,
        };
        let plan = plan_writeback(&[memory("Rust")], &[], &[document], &WritebackPolicy::default()).unwrap();
        let content = &plan.patches[0].content;
        assert!(content.contains("Manual before."));
        assert!(content.contains("Manual after."));
        assert!(content.contains("Rust"));
        assert!(!content.contains("\nold\n"));
    }

    #[test]
    fn identical_inputs_produce_no_change() {
        let first = plan_writeback(&[memory("Rust")], &[], &[], &WritebackPolicy::default()).unwrap();
        let document = MemoryDocument {
            path: first.patches[0].path.clone(),
            content: first.patches[0].content.clone(),
            expected_fingerprint: Some(first.patches[0].after_fingerprint.clone()),
        };
        let second = plan_writeback(&[memory("Rust")], &[], &[document], &WritebackPolicy::default()).unwrap();
        assert_eq!(second.patches[0].operation, WriteOperation::NoChange);
    }

    #[test]
    fn rejects_stale_document_snapshot() {
        let document = MemoryDocument {
            path: "memory/MEMORY.md".into(),
            content: "changed".into(),
            expected_fingerprint: Some("stale".into()),
        };
        assert!(plan_writeback(&[memory("Rust")], &[], &[document], &WritebackPolicy::default()).is_err());
    }

    #[test]
    fn output_is_independent_of_memory_input_order() {
        let mut second = memory("Cargo");
        second.key = "project\u{1f}build\u{1f}Fact".into();
        second.predicate = "build".into();
        let left = plan_writeback(&[memory("Rust"), second.clone()], &[], &[], &WritebackPolicy::default()).unwrap();
        let right = plan_writeback(&[second, memory("Rust")], &[], &[], &WritebackPolicy::default()).unwrap();
        assert_eq!(left.plan_fingerprint, right.plan_fingerprint);
        assert_eq!(left.patches, right.patches);
    }
}
