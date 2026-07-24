//! Deterministic, evidence-preserving context compaction.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use time::OffsetDateTime;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextKind {
    Goal,
    Constraint,
    Decision,
    Todo,
    Blocker,
    Failure,
    Evidence,
    Checkpoint,
    Observation,
    Conversation,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ContextItem {
    pub id: String,
    pub kind: ContextKind,
    pub content: String,
    pub sequence: u64,
    #[serde(with = "time::serde::rfc3339")]
    pub recorded_at: OffsetDateTime,
    #[serde(default)]
    pub references: BTreeSet<String>,
    #[serde(default)]
    pub terminal: bool,
}

impl ContextItem {
    pub fn new(
        id: impl Into<String>,
        kind: ContextKind,
        content: impl Into<String>,
        sequence: u64,
        recorded_at: OffsetDateTime,
    ) -> Result<Self, &'static str> {
        let id = id.into();
        let content = content.into();
        if id.trim().is_empty() {
            return Err("context item id cannot be empty");
        }
        if content.trim().is_empty() {
            return Err("context item content cannot be empty");
        }
        if sequence == 0 {
            return Err("context item sequence must start at one");
        }
        Ok(Self {
            id,
            kind,
            content,
            sequence,
            recorded_at,
            references: BTreeSet::new(),
            terminal: false,
        })
    }

    #[must_use]
    pub fn with_reference(mut self, reference: impl Into<String>) -> Self {
        let reference = reference.into();
        if !reference.trim().is_empty() {
            self.references.insert(reference);
        }
        self
    }

    #[must_use]
    pub fn terminal(mut self) -> Self {
        self.terminal = true;
        self
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CompactionPolicy {
    pub maximum_items: usize,
    pub maximum_bytes: usize,
    pub retain_recent_conversation: usize,
}

impl Default for CompactionPolicy {
    fn default() -> Self {
        Self {
            maximum_items: 128,
            maximum_bytes: 64 * 1024,
            retain_recent_conversation: 12,
        }
    }
}

impl CompactionPolicy {
    pub fn validate(self) -> Result<Self, &'static str> {
        if self.maximum_items == 0 {
            return Err("maximum_items must be greater than zero");
        }
        if self.maximum_bytes == 0 {
            return Err("maximum_bytes must be greater than zero");
        }
        Ok(self)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CompactedSection {
    pub kind: ContextKind,
    pub source_ids: Vec<String>,
    pub summary: String,
    pub fingerprint: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CompactionResult {
    pub sections: Vec<CompactedSection>,
    pub retained_items: Vec<ContextItem>,
    pub omitted_item_ids: Vec<String>,
    pub source_fingerprint: String,
    pub compacted_fingerprint: String,
    pub source_items: usize,
    pub compacted_items: usize,
    pub source_bytes: usize,
    pub compacted_bytes: usize,
}

impl CompactionResult {
    #[must_use]
    pub fn item_reduction_basis_points(&self) -> u16 {
        if self.source_items == 0 {
            return 0;
        }
        let removed = self.source_items.saturating_sub(self.compacted_items);
        ((removed.saturating_mul(10_000) / self.source_items).min(10_000)) as u16
    }

    #[must_use]
    pub fn byte_reduction_basis_points(&self) -> u16 {
        if self.source_bytes == 0 {
            return 0;
        }
        let removed = self.source_bytes.saturating_sub(self.compacted_bytes);
        ((removed.saturating_mul(10_000) / self.source_bytes).min(10_000)) as u16
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct ContextLedger {
    #[serde(default)]
    items: Vec<ContextItem>,
}

impl ContextLedger {
    pub fn append(&mut self, item: ContextItem) -> Result<(), &'static str> {
        let expected = self.items.len() as u64 + 1;
        if item.sequence != expected {
            return Err("context item sequence must be contiguous");
        }
        if self.items.iter().any(|existing| existing.id == item.id) {
            return Err("context item id must be unique");
        }
        if self
            .items
            .last()
            .is_some_and(|previous| item.recorded_at < previous.recorded_at)
        {
            return Err("context item timestamp regressed");
        }
        self.items.push(item);
        Ok(())
    }

    #[must_use]
    pub fn items(&self) -> &[ContextItem] {
        &self.items
    }

    pub fn validate(&self) -> Result<(), &'static str> {
        let mut ids = BTreeSet::new();
        for (index, item) in self.items.iter().enumerate() {
            if item.sequence != index as u64 + 1 {
                return Err("context ledger contains a non-contiguous sequence");
            }
            if !ids.insert(item.id.as_str()) {
                return Err("context ledger contains a duplicate id");
            }
            if index > 0 && item.recorded_at < self.items[index - 1].recorded_at {
                return Err("context ledger contains a timestamp regression");
            }
            for reference in &item.references {
                if !self.items.iter().any(|candidate| &candidate.id == reference) {
                    return Err("context item references an unknown id");
                }
            }
        }
        Ok(())
    }

    pub fn compact(&self, policy: CompactionPolicy) -> Result<CompactionResult, &'static str> {
        policy.validate()?;
        self.validate()?;

        let source_bytes = serialized_size(&self.items);
        let source_fingerprint = fingerprint_items(&self.items);
        if self.items.len() <= policy.maximum_items && source_bytes <= policy.maximum_bytes {
            return Ok(CompactionResult {
                sections: Vec::new(),
                retained_items: self.items.clone(),
                omitted_item_ids: Vec::new(),
                source_fingerprint: source_fingerprint.clone(),
                compacted_fingerprint: source_fingerprint,
                source_items: self.items.len(),
                compacted_items: self.items.len(),
                source_bytes,
                compacted_bytes: source_bytes,
            });
        }

        let recent_conversation: BTreeSet<&str> = self
            .items
            .iter()
            .rev()
            .filter(|item| item.kind == ContextKind::Conversation)
            .take(policy.retain_recent_conversation)
            .map(|item| item.id.as_str())
            .collect();

        let protected: BTreeSet<&str> = self
            .items
            .iter()
            .filter(|item| is_lossless_kind(item.kind) || item.terminal || recent_conversation.contains(item.id.as_str()))
            .map(|item| item.id.as_str())
            .collect();

        let retained_items: Vec<ContextItem> = self
            .items
            .iter()
            .filter(|item| protected.contains(item.id.as_str()))
            .cloned()
            .collect();

        let mut grouped: BTreeMap<ContextKind, Vec<&ContextItem>> = BTreeMap::new();
        for item in &self.items {
            if !protected.contains(item.id.as_str()) {
                grouped.entry(item.kind).or_default().push(item);
            }
        }

        let mut sections = Vec::new();
        let mut omitted_item_ids = Vec::new();
        for (kind, items) in grouped {
            let source_ids: Vec<String> = items.iter().map(|item| item.id.clone()).collect();
            omitted_item_ids.extend(source_ids.iter().cloned());
            let summary = deterministic_summary(&items);
            let fingerprint = fingerprint_text(&format!("{kind:?}\n{summary}"));
            sections.push(CompactedSection {
                kind,
                source_ids,
                summary,
                fingerprint,
            });
        }

        let compacted_items = retained_items.len() + sections.len();
        let compacted_bytes = serialized_size(&(sections.as_slice(), retained_items.as_slice()));
        if compacted_items > policy.maximum_items || compacted_bytes > policy.maximum_bytes {
            return Err("protected context exceeds compaction budget");
        }

        let compacted_fingerprint = fingerprint_text(&format!(
            "{}\n{}",
            fingerprint_sections(&sections),
            fingerprint_items(&retained_items)
        ));

        Ok(CompactionResult {
            sections,
            retained_items,
            omitted_item_ids,
            source_fingerprint,
            compacted_fingerprint,
            source_items: self.items.len(),
            compacted_items,
            source_bytes,
            compacted_bytes,
        })
    }
}

fn is_lossless_kind(kind: ContextKind) -> bool {
    matches!(
        kind,
        ContextKind::Goal
            | ContextKind::Constraint
            | ContextKind::Decision
            | ContextKind::Todo
            | ContextKind::Blocker
            | ContextKind::Failure
            | ContextKind::Evidence
            | ContextKind::Checkpoint
    )
}

fn deterministic_summary(items: &[&ContextItem]) -> String {
    items
        .iter()
        .map(|item| format!("[{}] {}", item.id, normalize(&item.content)))
        .collect::<Vec<_>>()
        .join("\n")
}

fn normalize(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn serialized_size<T: Serialize>(value: &T) -> usize {
    serde_json::to_vec(value).map_or(usize::MAX, |bytes| bytes.len())
}

fn fingerprint_items(items: &[ContextItem]) -> String {
    fingerprint_text(
        &items
            .iter()
            .map(|item| format!("{}|{:?}|{}|{}", item.sequence, item.kind, item.id, normalize(&item.content)))
            .collect::<Vec<_>>()
            .join("\n"),
    )
}

fn fingerprint_sections(sections: &[CompactedSection]) -> String {
    fingerprint_text(
        &sections
            .iter()
            .map(|section| format!("{:?}|{}|{}", section.kind, section.source_ids.join(","), section.summary))
            .collect::<Vec<_>>()
            .join("\n"),
    )
}

fn fingerprint_text(value: &str) -> String {
    hex::encode(Sha256::digest(value.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    fn item(sequence: u64, kind: ContextKind, content: &str) -> ContextItem {
        ContextItem::new(
            format!("item-{sequence}"),
            kind,
            content,
            sequence,
            datetime!(2026-07-24 10:00 UTC),
        )
        .expect("item")
    }

    #[test]
    fn preserves_goals_blockers_and_evidence_losslessly() {
        let mut ledger = ContextLedger::default();
        ledger.append(item(1, ContextKind::Goal, "finish the repository")).expect("append");
        ledger.append(item(2, ContextKind::Observation, "read file one")).expect("append");
        ledger.append(item(3, ContextKind::Blocker, "permission required")).expect("append");
        ledger.append(item(4, ContextKind::Evidence, "test output hash")).expect("append");
        ledger.append(item(5, ContextKind::Observation, "read file two")).expect("append");

        let result = ledger
            .compact(CompactionPolicy { maximum_items: 4, maximum_bytes: 4096, retain_recent_conversation: 0 })
            .expect("compact");

        assert!(result.retained_items.iter().any(|item| item.kind == ContextKind::Goal));
        assert!(result.retained_items.iter().any(|item| item.kind == ContextKind::Blocker));
        assert!(result.retained_items.iter().any(|item| item.kind == ContextKind::Evidence));
        assert_eq!(result.sections.len(), 1);
    }

    #[test]
    fn retains_only_the_configured_recent_conversation_tail() {
        let mut ledger = ContextLedger::default();
        for sequence in 1..=4 {
            ledger.append(item(sequence, ContextKind::Conversation, &format!("message {sequence}"))).expect("append");
        }
        let result = ledger
            .compact(CompactionPolicy { maximum_items: 3, maximum_bytes: 4096, retain_recent_conversation: 2 })
            .expect("compact");
        assert_eq!(result.retained_items.len(), 2);
        assert_eq!(result.retained_items[0].id, "item-3");
        assert_eq!(result.retained_items[1].id, "item-4");
    }

    #[test]
    fn compaction_is_deterministic() {
        let mut ledger = ContextLedger::default();
        for sequence in 1..=5 {
            ledger.append(item(sequence, ContextKind::Observation, &format!("observation   {sequence}"))).expect("append");
        }
        let policy = CompactionPolicy { maximum_items: 2, maximum_bytes: 4096, retain_recent_conversation: 0 };
        let first = ledger.compact(policy).expect("first");
        let second = ledger.compact(policy).expect("second");
        assert_eq!(first, second);
    }

    #[test]
    fn unknown_references_are_rejected() {
        let mut ledger = ContextLedger::default();
        ledger
            .append(item(1, ContextKind::Observation, "one").with_reference("missing"))
            .expect("append");
        assert_eq!(ledger.validate(), Err("context item references an unknown id"));
    }

    #[test]
    fn protected_context_cannot_be_silently_dropped() {
        let mut ledger = ContextLedger::default();
        ledger.append(item(1, ContextKind::Goal, &"x".repeat(1000))).expect("append");
        assert_eq!(
            ledger.compact(CompactionPolicy { maximum_items: 1, maximum_bytes: 10, retain_recent_conversation: 0 }),
            Err("protected context exceeds compaction budget")
        );
    }
}
