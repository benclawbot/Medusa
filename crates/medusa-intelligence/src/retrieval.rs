use std::{cmp::Reverse, collections::BTreeSet, fs, path::PathBuf};

use serde::{Deserialize, Serialize};

use crate::{CodeIndex, Symbol};

const CHARS_PER_TOKEN: usize = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetrievalBudget {
    pub max_tokens: usize,
    pub max_results: usize,
    pub max_tokens_per_result: usize,
}

impl Default for RetrievalBudget {
    fn default() -> Self {
        Self {
            max_tokens: 8_000,
            max_results: 24,
            max_tokens_per_result: 1_200,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetrievalResult {
    pub path: PathBuf,
    pub symbol: String,
    pub start_line: usize,
    pub end_line: usize,
    pub score: u32,
    pub estimated_tokens: usize,
    pub content: String,
    pub reasons: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetrievalExclusion {
    pub path: PathBuf,
    pub symbol: String,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetrievalReport {
    pub query: String,
    pub budget: RetrievalBudget,
    pub used_tokens: usize,
    pub results: Vec<RetrievalResult>,
    pub exclusions: Vec<RetrievalExclusion>,
}

impl CodeIndex {
    pub fn retrieve(
        &self,
        repo: &std::path::Path,
        query: &str,
        budget: RetrievalBudget,
    ) -> RetrievalReport {
        let terms = normalized_terms(query);
        let mut candidates = self
            .symbols
            .iter()
            .filter_map(|symbol| rank_symbol(symbol, &terms))
            .collect::<Vec<_>>();
        candidates.sort_by_key(|candidate| {
            (
                Reverse(candidate.score),
                candidate.symbol.path.clone(),
                candidate.symbol.start_byte,
                candidate.symbol.name.clone(),
            )
        });

        let mut used_tokens: usize = 0;
        let mut results = Vec::new();
        let mut exclusions = Vec::new();
        for candidate in candidates {
            if results.len() >= budget.max_results {
                exclusions.push(exclusion(candidate.symbol, "result limit reached"));
                continue;
            }
            let path = repo.join(&candidate.symbol.path);
            let Ok(source) = fs::read_to_string(&path) else {
                exclusions.push(exclusion(candidate.symbol, "source unavailable"));
                continue;
            };
            let Some(fragment) = source.get(candidate.symbol.start_byte..candidate.symbol.end_byte)
            else {
                exclusions.push(exclusion(candidate.symbol, "symbol byte range is stale"));
                continue;
            };
            let estimated_tokens = estimate_tokens(fragment);
            if estimated_tokens > budget.max_tokens_per_result {
                exclusions.push(exclusion(
                    candidate.symbol,
                    "per-result token limit exceeded",
                ));
                continue;
            }
            if used_tokens.saturating_add(estimated_tokens) > budget.max_tokens {
                exclusions.push(exclusion(candidate.symbol, "total token budget exhausted"));
                continue;
            }
            used_tokens += estimated_tokens;
            results.push(RetrievalResult {
                path: candidate.symbol.path.clone(),
                symbol: candidate.symbol.name.clone(),
                start_line: candidate.symbol.start_line,
                end_line: candidate.symbol.end_line,
                score: candidate.score,
                estimated_tokens,
                content: fragment.to_owned(),
                reasons: candidate.reasons,
            });
        }

        RetrievalReport {
            query: query.to_owned(),
            budget,
            used_tokens,
            results,
            exclusions,
        }
    }
}

struct RankedSymbol<'a> {
    symbol: &'a Symbol,
    score: u32,
    reasons: Vec<String>,
}

fn rank_symbol<'a>(symbol: &'a Symbol, terms: &[String]) -> Option<RankedSymbol<'a>> {
    if terms.is_empty() {
        return None;
    }
    let name = symbol.name.to_ascii_lowercase();
    let path = symbol.path.to_string_lossy().to_ascii_lowercase();
    let mut score = 0;
    let mut reasons = Vec::new();
    for term in terms {
        if name == *term {
            score += 100;
            reasons.push(format!("exact symbol match: {term}"));
        } else if name.starts_with(term) {
            score += 60;
            reasons.push(format!("symbol prefix match: {term}"));
        } else if name.contains(term) {
            score += 40;
            reasons.push(format!("symbol contains: {term}"));
        }
        if path.ends_with(term) || path.contains(&format!("/{term}.")) {
            score += 30;
            reasons.push(format!("filename match: {term}"));
        } else if path.contains(term) {
            score += 15;
            reasons.push(format!("path match: {term}"));
        }
    }
    (score > 0).then_some(RankedSymbol {
        symbol,
        score,
        reasons,
    })
}

fn normalized_terms(query: &str) -> Vec<String> {
    query
        .split(|character: char| !character.is_ascii_alphanumeric() && character != '_')
        .filter(|term| term.len() >= 2)
        .map(str::to_ascii_lowercase)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn estimate_tokens(content: &str) -> usize {
    content.len().div_ceil(CHARS_PER_TOKEN).max(1)
}

fn exclusion(symbol: &Symbol, reason: &str) -> RetrievalExclusion {
    RetrievalExclusion {
        path: symbol.path.clone(),
        symbol: symbol.name.clone(),
        reason: reason.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn exact_symbol_matches_rank_first_deterministically() {
        let directory = tempfile::tempdir().expect("tempdir");
        fs::write(
            directory.path().join("lib.rs"),
            "pub fn session_budget() -> usize { 8 }\npub fn budget_helper() -> usize { 4 }\n",
        )
        .expect("source");
        let index = CodeIndex::build(directory.path()).expect("index");
        let report = index.retrieve(
            directory.path(),
            "session_budget",
            RetrievalBudget::default(),
        );
        assert_eq!(report.results[0].symbol, "session_budget");
        assert!(report.results[0].score > report.results[1].score);
        assert_eq!(
            report.used_tokens,
            report.results.iter().map(|r| r.estimated_tokens).sum()
        );
    }

    #[test]
    fn hard_budget_excludes_lower_ranked_context_with_reason() {
        let directory = tempfile::tempdir().expect("tempdir");
        fs::write(
            directory.path().join("lib.rs"),
            "pub fn retrieve_alpha() -> usize { 1 }\npub fn retrieve_beta() -> usize { 2 }\n",
        )
        .expect("source");
        let index = CodeIndex::build(directory.path()).expect("index");
        let report = index.retrieve(
            directory.path(),
            "retrieve",
            RetrievalBudget {
                max_tokens: 12,
                max_results: 8,
                max_tokens_per_result: 12,
            },
        );
        assert!(report.used_tokens <= 12);
        assert_eq!(report.results.len(), 1);
        assert!(
            report
                .exclusions
                .iter()
                .any(|item| item.reason == "total token budget exhausted")
        );
    }
}
