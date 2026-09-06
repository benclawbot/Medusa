//! Deterministic retrieval of the smallest relevant context working set.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use medusa_context::{ContextItem, ContextKind, ContextLedger};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RetrievalQuery {
    pub text: String,
    #[serde(default)]
    pub required_ids: BTreeSet<String>,
    #[serde(default)]
    pub preferred_kinds: BTreeSet<ContextKind>,
    pub maximum_items: usize,
    pub maximum_bytes: usize,
}

impl RetrievalQuery {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.text.trim().is_empty() && self.required_ids.is_empty() {
            return Err("retrieval query must include text or required ids");
        }
        if self.maximum_items == 0 {
            return Err("maximum_items must be greater than zero");
        }
        if self.maximum_bytes == 0 {
            return Err("maximum_bytes must be greater than zero");
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RetrievedItem {
    pub item: ContextItem,
    pub score: u32,
    pub reasons: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RetrievalResult {
    pub items: Vec<RetrievedItem>,
    pub omitted_ids: Vec<String>,
    pub total_bytes: usize,
    pub fingerprint: String,
}

impl RetrievalResult {
    #[must_use]
    pub fn ids(&self) -> Vec<&str> {
        self.items
            .iter()
            .map(|entry| entry.item.id.as_str())
            .collect()
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ContextRetriever;

/// Bounded memo cache for retrieval results, keyed by a fingerprint of the
/// query plus the ledger contents. Repeated identical turns (retries,
/// multi-agent fan-out over the same ledger) skip the full scan; the ledger
/// fingerprint keeps stale hits impossible.
const DEFAULT_MEMO_CAPACITY: usize = 64;

#[derive(Clone, Debug)]
pub struct RetrievalMemo {
    capacity: usize,
    entries: std::collections::HashMap<String, RetrievalResult>,
}

impl Default for RetrievalMemo {
    fn default() -> Self {
        Self::new(DEFAULT_MEMO_CAPACITY)
    }
}

impl RetrievalMemo {
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            entries: std::collections::HashMap::new(),
        }
    }

    pub fn retrieve_cached(
        &mut self,
        ledger: &ContextLedger,
        query: &RetrievalQuery,
    ) -> Result<RetrievalResult, &'static str> {
        // Validate before consulting the cache. A key hit must never turn an invalid
        // request or ledger into a successful retrieval.
        query.validate()?;
        ledger.validate()?;
        let key = memo_key(ledger, query);
        if let Some(hit) = self.entries.get(&key) {
            return Ok(hit.clone());
        }
        let result = ContextRetriever.retrieve(ledger, query)?;
        if self.entries.len() >= self.capacity {
            // Evict an arbitrary entry; insertion order tracking is not worth
            // the bookkeeping for a small memo.
            if let Some(first) = self.entries.keys().next().cloned() {
                self.entries.remove(&first);
            }
        }
        self.entries.insert(key, result.clone());
        Ok(result)
    }
}

fn memo_key(ledger: &ContextLedger, query: &RetrievalQuery) -> String {
    let mut hasher = Sha256::new();
    update_framed(&mut hasher, b"medusa-retrieval-memo-v2");
    // Serialize the typed query so every field, including collection boundaries and
    // enum discriminants, participates in the identity.
    let query_bytes = serde_json::to_vec(query).expect("retrieval query is serializable");
    update_framed(&mut hasher, &query_bytes);
    for item in ledger.items() {
        let item_bytes = serde_json::to_vec(item).expect("context item is serializable");
        update_framed(&mut hasher, &item_bytes);
    }
    format!("{:x}", hasher.finalize())
}

fn update_framed(hasher: &mut Sha256, value: &[u8]) {
    hasher.update((value.len() as u64).to_le_bytes());
    hasher.update(value);
}

impl ContextRetriever {
    pub fn retrieve(
        &self,
        ledger: &ContextLedger,
        query: &RetrievalQuery,
    ) -> Result<RetrievalResult, &'static str> {
        query.validate()?;
        ledger.validate()?;

        let by_id: BTreeMap<&str, &ContextItem> = ledger
            .items()
            .iter()
            .map(|item| (item.id.as_str(), item))
            .collect();
        for required in &query.required_ids {
            if !by_id.contains_key(required.as_str()) {
                return Err("required context id does not exist");
            }
        }

        let query_tokens = tokenize(&query.text);
        let mut candidates: Vec<RetrievedItem> = ledger
            .items()
            .iter()
            .map(|item| score_item(item, &query_tokens, query))
            .collect();

        candidates.sort_by(|left, right| {
            right
                .score
                .cmp(&left.score)
                .then_with(|| right.item.terminal.cmp(&left.item.terminal))
                .then_with(|| right.item.sequence.cmp(&left.item.sequence))
                .then_with(|| left.item.id.cmp(&right.item.id))
        });

        let mut selected = BTreeSet::new();
        let mut queue: VecDeque<String> = query.required_ids.iter().cloned().collect();
        while let Some(id) = queue.pop_front() {
            if !selected.insert(id.clone()) {
                continue;
            }
            let item = by_id
                .get(id.as_str())
                .ok_or("context reference does not exist")?;
            for reference in &item.references {
                queue.push_back(reference.clone());
            }
        }

        let required_bytes: usize = selected
            .iter()
            .map(|id| serialized_size(by_id[id.as_str()]))
            .sum();
        if selected.len() > query.maximum_items || required_bytes > query.maximum_bytes {
            return Err("required context exceeds retrieval budget");
        }

        let mut used_bytes = required_bytes;
        for candidate in &candidates {
            if selected.contains(&candidate.item.id) || candidate.score == 0 {
                continue;
            }
            let mut closure = BTreeSet::new();
            collect_reference_closure(&candidate.item, &by_id, &selected, &mut closure)?;
            closure.insert(candidate.item.id.clone());
            let incremental_bytes: usize = closure
                .iter()
                .filter(|id| !selected.contains(*id))
                .map(|id| serialized_size(by_id[id.as_str()]))
                .sum();
            let incremental_items = closure.iter().filter(|id| !selected.contains(*id)).count();
            if selected.len() + incremental_items > query.maximum_items
                || used_bytes + incremental_bytes > query.maximum_bytes
            {
                continue;
            }
            used_bytes += incremental_bytes;
            selected.extend(closure);
        }

        let mut items: Vec<RetrievedItem> = candidates
            .into_iter()
            .filter(|entry| selected.contains(&entry.item.id))
            .collect();
        items.sort_by_key(|entry| entry.item.sequence);
        let omitted_ids = ledger
            .items()
            .iter()
            .filter(|item| !selected.contains(&item.id))
            .map(|item| item.id.clone())
            .collect();
        let fingerprint = fingerprint(&items);

        Ok(RetrievalResult {
            items,
            omitted_ids,
            total_bytes: used_bytes,
            fingerprint,
        })
    }
}

fn score_item(
    item: &ContextItem,
    query_tokens: &BTreeSet<String>,
    query: &RetrievalQuery,
) -> RetrievedItem {
    let item_tokens = tokenize(&item.content);
    let overlap = query_tokens.intersection(&item_tokens).count() as u32;
    let mut score = overlap.saturating_mul(100);
    let mut reasons = Vec::new();

    if overlap > 0 {
        reasons.push(format!("{overlap} lexical matches"));
    }
    if query.required_ids.contains(&item.id) {
        score = score.saturating_add(10_000);
        reasons.push("explicitly required".to_owned());
    }
    if query.preferred_kinds.contains(&item.kind) {
        score = score.saturating_add(500);
        reasons.push("preferred context kind".to_owned());
    }
    if item.terminal {
        score = score.saturating_add(250);
        reasons.push("terminal state".to_owned());
    }
    if matches!(
        item.kind,
        ContextKind::Goal | ContextKind::Blocker | ContextKind::Checkpoint
    ) {
        score = score.saturating_add(100);
        reasons.push("execution-critical kind".to_owned());
    }

    RetrievedItem {
        item: item.clone(),
        score,
        reasons,
    }
}

fn tokenize(value: &str) -> BTreeSet<String> {
    value
        .split(|character: char| !character.is_alphanumeric())
        .filter(|token| token.len() >= 2)
        .map(str::to_lowercase)
        .collect()
}

fn collect_reference_closure(
    item: &ContextItem,
    by_id: &BTreeMap<&str, &ContextItem>,
    selected: &BTreeSet<String>,
    closure: &mut BTreeSet<String>,
) -> Result<(), &'static str> {
    for reference in &item.references {
        if selected.contains(reference) || !closure.insert(reference.clone()) {
            continue;
        }
        let referenced = by_id
            .get(reference.as_str())
            .ok_or("context reference does not exist")?;
        collect_reference_closure(referenced, by_id, selected, closure)?;
    }
    Ok(())
}

fn serialized_size(item: &ContextItem) -> usize {
    serde_json::to_vec(item).map_or(usize::MAX, |bytes| bytes.len())
}

fn fingerprint(items: &[RetrievedItem]) -> String {
    let mut hasher = Sha256::new();
    for entry in items {
        hasher.update(entry.item.id.as_bytes());
        hasher.update([0]);
        hasher.update(entry.item.sequence.to_le_bytes());
        hasher.update(entry.score.to_le_bytes());
        hasher.update(entry.item.content.as_bytes());
        hasher.update([0xff]);
    }
    hex::encode(hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    fn item(id: &str, kind: ContextKind, content: &str, sequence: u64) -> ContextItem {
        ContextItem::new(id, kind, content, sequence, datetime!(2026-07-24 12:00 UTC))
            .expect("valid item")
    }

    #[test]
    fn retrieves_relevant_items_in_source_order() {
        let mut ledger = ContextLedger::default();
        ledger
            .append(item(
                "goal",
                ContextKind::Goal,
                "finish retry controller",
                1,
            ))
            .unwrap();
        ledger
            .append(item(
                "note",
                ContextKind::Observation,
                "unrelated styling note",
                2,
            ))
            .unwrap();
        ledger
            .append(item(
                "todo",
                ContextKind::Todo,
                "implement retry controller tests",
                3,
            ))
            .unwrap();
        let query = RetrievalQuery {
            text: "retry controller".to_owned(),
            required_ids: BTreeSet::new(),
            preferred_kinds: BTreeSet::new(),
            maximum_items: 2,
            maximum_bytes: 4096,
        };
        let result = ContextRetriever.retrieve(&ledger, &query).unwrap();
        assert_eq!(result.ids(), vec!["goal", "todo"]);
    }

    #[test]
    fn memo_returns_cached_result_for_identical_query() {
        let mut ledger = ContextLedger::default();
        ledger
            .append(item(
                "goal",
                ContextKind::Goal,
                "finish retry controller",
                1,
            ))
            .unwrap();
        let query = RetrievalQuery {
            text: "retry controller".to_owned(),
            required_ids: BTreeSet::new(),
            preferred_kinds: BTreeSet::new(),
            maximum_items: 4,
            maximum_bytes: 4096,
        };
        let mut memo = RetrievalMemo::new(8);
        let first = memo.retrieve_cached(&ledger, &query).unwrap();
        let second = memo.retrieve_cached(&ledger, &query).unwrap();
        assert_eq!(first, second);
        assert_eq!(first.ids(), vec!["goal"]);
    }

    #[test]
    fn memo_key_includes_context_content_not_only_ids_and_sequences() {
        let mut first_ledger = ContextLedger::default();
        first_ledger
            .append(item(
                "note",
                ContextKind::Observation,
                "first version",
                1,
            ))
            .unwrap();
        let mut second_ledger = ContextLedger::default();
        second_ledger
            .append(item(
                "note",
                ContextKind::Observation,
                "second version",
                1,
            ))
            .unwrap();
        let query = RetrievalQuery {
            text: "version".to_owned(),
            required_ids: BTreeSet::new(),
            preferred_kinds: BTreeSet::new(),
            maximum_items: 4,
            maximum_bytes: 4096,
        };
        let mut memo = RetrievalMemo::new(2);
        let first = memo.retrieve_cached(&first_ledger, &query).unwrap();
        let second = memo.retrieve_cached(&second_ledger, &query).unwrap();

        assert_ne!(first.fingerprint, second.fingerprint);
        assert_eq!(first.items[0].item.content, "first version");
        assert_eq!(second.items[0].item.content, "second version");
    }

    #[test]
    fn memo_validates_before_returning_a_cached_hit() {
        let mut ledger = ContextLedger::default();
        ledger
            .append(item(
                "note",
                ContextKind::Observation,
                "memoized content",
                1,
            ))
            .unwrap();
        let valid_query = RetrievalQuery {
            text: "memoized".to_owned(),
            required_ids: BTreeSet::new(),
            preferred_kinds: BTreeSet::new(),
            maximum_items: 4,
            maximum_bytes: 4096,
        };
        let mut memo = RetrievalMemo::new(2);
        memo.retrieve_cached(&ledger, &valid_query).unwrap();

        let invalid_query = RetrievalQuery {
            text: String::new(),
            ..valid_query
        };
        assert_eq!(
            memo.retrieve_cached(&ledger, &invalid_query),
            Err("retrieval query must include text or required ids")
        );
    }

    #[test]
    fn default_and_explicit_memo_capacity_have_consistent_semantics() {
        assert_eq!(RetrievalMemo::default().capacity, DEFAULT_MEMO_CAPACITY);
        assert_eq!(RetrievalMemo::new(0).capacity, RetrievalMemo::new(1).capacity);
    }

    #[test]
    fn reference_closure_is_preserved() {
        let mut ledger = ContextLedger::default();
        ledger
            .append(item("goal", ContextKind::Goal, "ship release", 1))
            .unwrap();
        ledger
            .append(
                item("evidence", ContextKind::Evidence, "release tests passed", 2)
                    .with_reference("goal"),
            )
            .unwrap();
        let query = RetrievalQuery {
            text: "tests passed".to_owned(),
            required_ids: BTreeSet::new(),
            preferred_kinds: BTreeSet::new(),
            maximum_items: 2,
            maximum_bytes: 4096,
        };
        let result = ContextRetriever.retrieve(&ledger, &query).unwrap();
        assert_eq!(result.ids(), vec!["goal", "evidence"]);
    }

    #[test]
    fn required_context_cannot_be_silently_dropped() {
        let mut ledger = ContextLedger::default();
        ledger
            .append(item("goal", ContextKind::Goal, "large goal body", 1))
            .unwrap();
        let query = RetrievalQuery {
            text: String::new(),
            required_ids: BTreeSet::from(["goal".to_owned()]),
            preferred_kinds: BTreeSet::new(),
            maximum_items: 1,
            maximum_bytes: 1,
        };
        assert_eq!(
            ContextRetriever.retrieve(&ledger, &query),
            Err("required context exceeds retrieval budget")
        );
    }

    #[test]
    fn identical_inputs_produce_identical_fingerprints() {
        let mut ledger = ContextLedger::default();
        ledger
            .append(item(
                "goal",
                ContextKind::Goal,
                "finish context retrieval",
                1,
            ))
            .unwrap();
        let query = RetrievalQuery {
            text: "context retrieval".to_owned(),
            required_ids: BTreeSet::new(),
            preferred_kinds: BTreeSet::new(),
            maximum_items: 4,
            maximum_bytes: 4096,
        };
        let first = ContextRetriever.retrieve(&ledger, &query).unwrap();
        let second = ContextRetriever.retrieve(&ledger, &query).unwrap();
        assert_eq!(first.fingerprint, second.fingerprint);
    }
}
