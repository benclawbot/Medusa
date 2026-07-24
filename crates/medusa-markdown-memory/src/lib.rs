//! Deterministic Markdown-memory indexing and retrieval.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct MemoryDocument {
    pub path: String,
    pub content: String,
    pub revision: u64,
}

impl MemoryDocument {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.path.trim().is_empty() { return Err("memory path cannot be empty"); }
        if !self.path.ends_with(".md") { return Err("memory document must be markdown"); }
        if self.content.trim().is_empty() { return Err("memory content cannot be empty"); }
        if self.revision == 0 { return Err("memory revision must start at one"); }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct MemoryChunk {
    pub id: String,
    pub path: String,
    pub heading: String,
    pub body: String,
    pub ordinal: usize,
    pub revision: u64,
    pub terms: BTreeMap<String, u16>,
    pub fingerprint: String,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct MemoryIndex {
    documents: BTreeMap<String, String>,
    chunks: BTreeMap<String, MemoryChunk>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RefreshReport {
    pub path: String,
    pub changed: bool,
    pub removed_chunks: usize,
    pub added_chunks: usize,
    pub document_fingerprint: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RetrievalQuery {
    pub text: String,
    pub maximum_results: usize,
    pub maximum_bytes: usize,
    #[serde(default)]
    pub preferred_paths: BTreeSet<String>,
    #[serde(default)]
    pub required_chunk_ids: BTreeSet<String>,
}

impl RetrievalQuery {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.text.trim().is_empty() { return Err("retrieval query cannot be empty"); }
        if self.maximum_results == 0 { return Err("maximum_results must be greater than zero"); }
        if self.maximum_bytes == 0 { return Err("maximum_bytes must be greater than zero"); }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct MemoryHit {
    pub chunk: MemoryChunk,
    pub score: u64,
    pub reasons: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RetrievalResult {
    pub hits: Vec<MemoryHit>,
    pub omitted_chunk_ids: Vec<String>,
    pub selected_bytes: usize,
    pub index_fingerprint: String,
    pub query_fingerprint: String,
    pub result_fingerprint: String,
}

impl MemoryIndex {
    pub fn refresh(&mut self, document: MemoryDocument) -> Result<RefreshReport, &'static str> {
        document.validate()?;
        let document_fingerprint = fingerprint(document.content.as_bytes());
        let changed = self.documents.get(&document.path) != Some(&document_fingerprint);
        if !changed {
            return Ok(RefreshReport { path: document.path, changed: false, removed_chunks: 0, added_chunks: 0, document_fingerprint });
        }

        let old_ids: Vec<String> = self.chunks.values().filter(|chunk| chunk.path == document.path).map(|chunk| chunk.id.clone()).collect();
        for id in &old_ids { self.chunks.remove(id); }

        let chunks = parse_markdown(&document)?;
        let added_chunks = chunks.len();
        for chunk in chunks {
            if self.chunks.insert(chunk.id.clone(), chunk).is_some() {
                return Err("memory chunk id collision");
            }
        }
        self.documents.insert(document.path.clone(), document_fingerprint.clone());
        Ok(RefreshReport { path: document.path, changed: true, removed_chunks: old_ids.len(), added_chunks, document_fingerprint })
    }

    pub fn remove(&mut self, path: &str) -> bool {
        let existed = self.documents.remove(path).is_some();
        self.chunks.retain(|_, chunk| chunk.path != path);
        existed
    }

    pub fn validate(&self) -> Result<(), &'static str> {
        for chunk in self.chunks.values() {
            if !self.documents.contains_key(&chunk.path) { return Err("chunk references an unknown document"); }
            if chunk.body.trim().is_empty() { return Err("chunk body cannot be empty"); }
            if chunk.fingerprint != chunk_fingerprint(&chunk.path, &chunk.heading, &chunk.body, chunk.ordinal, chunk.revision) {
                return Err("chunk fingerprint mismatch");
            }
        }
        Ok(())
    }

    #[must_use]
    pub fn fingerprint(&self) -> String {
        let mut hasher = Sha256::new();
        for (path, doc_hash) in &self.documents {
            hasher.update(path.as_bytes());
            hasher.update([0]);
            hasher.update(doc_hash.as_bytes());
            hasher.update([0]);
        }
        for chunk in self.chunks.values() { hasher.update(chunk.fingerprint.as_bytes()); hasher.update([0]); }
        hex::encode(hasher.finalize())
    }

    pub fn retrieve(&self, query: &RetrievalQuery) -> Result<RetrievalResult, &'static str> {
        query.validate()?;
        self.validate()?;
        for required in &query.required_chunk_ids {
            if !self.chunks.contains_key(required) { return Err("required memory chunk does not exist"); }
        }

        let query_terms = term_counts(&query.text);
        let mut ranked: Vec<MemoryHit> = self.chunks.values().map(|chunk| {
            let mut score = 0u64;
            let mut reasons = Vec::new();
            for (term, query_count) in &query_terms {
                if let Some(chunk_count) = chunk.terms.get(term) {
                    score += u64::from(*query_count) * u64::from(*chunk_count) * 100;
                }
            }
            if score > 0 { reasons.push("lexical_overlap".to_owned()); }
            if query.preferred_paths.contains(&chunk.path) { score += 2_000; reasons.push("preferred_path".to_owned()); }
            if query.required_chunk_ids.contains(&chunk.id) { score += 1_000_000; reasons.push("required".to_owned()); }
            if chunk.heading.to_lowercase().contains(&query.text.to_lowercase()) { score += 1_000; reasons.push("heading_match".to_owned()); }
            MemoryHit { chunk: chunk.clone(), score, reasons }
        }).filter(|hit| hit.score > 0).collect();

        ranked.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| a.chunk.path.cmp(&b.chunk.path)).then_with(|| a.chunk.ordinal.cmp(&b.chunk.ordinal)).then_with(|| a.chunk.id.cmp(&b.chunk.id)));

        let mut hits = Vec::new();
        let mut selected_bytes = 0usize;
        let mut omitted = Vec::new();
        for hit in ranked {
            let bytes = hit.chunk.heading.len() + hit.chunk.body.len();
            let required = query.required_chunk_ids.contains(&hit.chunk.id);
            if hits.len() >= query.maximum_results || selected_bytes.saturating_add(bytes) > query.maximum_bytes {
                if required { return Err("required memory chunk cannot fit retrieval budget"); }
                omitted.push(hit.chunk.id);
                continue;
            }
            selected_bytes += bytes;
            hits.push(hit);
        }

        let index_fingerprint = self.fingerprint();
        let query_fingerprint = fingerprint(format!("{}\0{}\0{}\0{:?}\0{:?}", query.text, query.maximum_results, query.maximum_bytes, query.preferred_paths, query.required_chunk_ids).as_bytes());
        let result_fingerprint = fingerprint(format!("{}\0{}\0{:?}", index_fingerprint, query_fingerprint, hits.iter().map(|hit| (&hit.chunk.id, hit.score)).collect::<Vec<_>>()).as_bytes());
        Ok(RetrievalResult { hits, omitted_chunk_ids: omitted, selected_bytes, index_fingerprint, query_fingerprint, result_fingerprint })
    }
}

fn parse_markdown(document: &MemoryDocument) -> Result<Vec<MemoryChunk>, &'static str> {
    let mut sections: Vec<(String, Vec<String>)> = Vec::new();
    let mut heading = "Document".to_owned();
    let mut body = Vec::new();
    for line in document.content.lines() {
        if let Some(title) = line.strip_prefix('#') {
            if !body.iter().all(|line: &String| line.trim().is_empty()) { sections.push((heading, std::mem::take(&mut body))); }
            heading = title.trim_start_matches('#').trim().to_owned();
            if heading.is_empty() { heading = "Untitled".to_owned(); }
        } else { body.push(line.to_owned()); }
    }
    if !body.iter().all(|line| line.trim().is_empty()) { sections.push((heading, body)); }
    if sections.is_empty() { return Err("markdown document contains no retrievable content"); }

    Ok(sections.into_iter().enumerate().map(|(ordinal, (heading, lines))| {
        let body = lines.join("\n").trim().to_owned();
        let id = format!("{}#{}-{}", document.path, ordinal + 1, slug(&heading));
        MemoryChunk { fingerprint: chunk_fingerprint(&document.path, &heading, &body, ordinal, document.revision), terms: term_counts(&format!("{heading} {body}")), id, path: document.path.clone(), heading, body, ordinal, revision: document.revision }
    }).collect())
}

fn slug(value: &str) -> String {
    value.to_lowercase().chars().map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '-' }).collect::<String>().split('-').filter(|part| !part.is_empty()).collect::<Vec<_>>().join("-")
}

fn term_counts(value: &str) -> BTreeMap<String, u16> {
    let mut counts = BTreeMap::new();
    for term in value.to_lowercase().split(|ch: char| !ch.is_ascii_alphanumeric()).filter(|term| term.len() > 1) {
        let entry = counts.entry(term.to_owned()).or_insert(0u16);
        *entry = entry.saturating_add(1);
    }
    counts
}

fn chunk_fingerprint(path: &str, heading: &str, body: &str, ordinal: usize, revision: u64) -> String {
    fingerprint(format!("{path}\0{heading}\0{body}\0{ordinal}\0{revision}").as_bytes())
}

fn fingerprint(bytes: &[u8]) -> String { hex::encode(Sha256::digest(bytes)) }

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(content: &str, revision: u64) -> MemoryDocument { MemoryDocument { path: "memory/project.md".into(), content: content.into(), revision } }

    #[test]
    fn retrieves_relevant_section_deterministically() {
        let mut index = MemoryIndex::default();
        index.refresh(doc("# Goal\nShip retry logic\n# Preference\nUse Rust", 1)).unwrap();
        let query = RetrievalQuery { text: "retry logic".into(), maximum_results: 3, maximum_bytes: 1_000, preferred_paths: BTreeSet::new(), required_chunk_ids: BTreeSet::new() };
        let first = index.retrieve(&query).unwrap();
        let second = index.retrieve(&query).unwrap();
        assert_eq!(first, second);
        assert_eq!(first.hits[0].chunk.heading, "Goal");
    }

    #[test]
    fn unchanged_document_does_not_reindex() {
        let mut index = MemoryIndex::default();
        assert!(index.refresh(doc("# Goal\nShip", 1)).unwrap().changed);
        assert!(!index.refresh(doc("# Goal\nShip", 2)).unwrap().changed);
    }

    #[test]
    fn changed_document_replaces_old_chunks() {
        let mut index = MemoryIndex::default();
        index.refresh(doc("# Old\nalpha", 1)).unwrap();
        let report = index.refresh(doc("# New\nbeta", 2)).unwrap();
        assert_eq!(report.removed_chunks, 1);
        assert_eq!(report.added_chunks, 1);
        assert!(index.retrieve(&RetrievalQuery { text: "alpha".into(), maximum_results: 2, maximum_bytes: 100, preferred_paths: BTreeSet::new(), required_chunk_ids: BTreeSet::new() }).unwrap().hits.is_empty());
    }

    #[test]
    fn required_chunk_fails_when_budget_is_too_small() {
        let mut index = MemoryIndex::default();
        index.refresh(doc("# Goal\nA long protected memory", 1)).unwrap();
        let id = index.chunks.keys().next().unwrap().clone();
        let error = index.retrieve(&RetrievalQuery { text: "protected".into(), maximum_results: 1, maximum_bytes: 1, preferred_paths: BTreeSet::new(), required_chunk_ids: BTreeSet::from([id]) }).unwrap_err();
        assert_eq!(error, "required memory chunk cannot fit retrieval budget");
    }
}
