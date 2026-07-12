use std::cmp::Reverse;

use medusa_core::MedusaResult;
use time::OffsetDateTime;

use crate::{
    engine::MemoryEngine,
    schema::{MemoryDocument, RetrievedMemory, Scope, Status, Validation},
    support::{normalize, tokenize},
};

impl MemoryEngine {
    /// Retrieves only active, non-expired, high-confidence memory by deterministic score.
    pub fn search(
        &self,
        query: &str,
        scope: Scope,
        limit: usize,
    ) -> MedusaResult<Vec<RetrievedMemory>> {
        let terms = tokenize(query);
        let now = OffsetDateTime::now_utc();
        let mut results = self
            .documents()?
            .into_iter()
            .filter(|(_, document)| {
                document.scope == scope
                    && document.status == Status::Active
                    && document.validation.high_confidence()
                    && !document.expired(now)
            })
            .filter_map(|(path, document)| {
                let score = score(&document, &terms);
                (score > 0).then_some(RetrievedMemory {
                    document,
                    path,
                    score,
                })
            })
            .collect::<Vec<_>>();
        results.sort_by_key(|result| {
            (
                Reverse(result.score),
                result.document.id.clone(),
                result.path.clone(),
            )
        });
        results.truncate(limit);
        Ok(results)
    }
}

fn score(document: &MemoryDocument, terms: &[String]) -> i64 {
    let title = normalize(&document.title);
    let body = normalize(&document.body);
    let tags = document
        .tags
        .iter()
        .map(|tag| normalize(tag))
        .collect::<Vec<_>>();
    let mut score = i64::from(document.confidence_milli) / 10;
    score += i64::from(document.successful_reuse_count) * 25;
    score += match document.validation {
        Validation::TestVerified => 80,
        Validation::UserStated | Validation::SourceVerified => 70,
        Validation::Observed => 60,
        _ => -500,
    };
    for term in terms {
        if title.contains(term) {
            score += 120;
        }
        if body.contains(term) {
            score += 60;
        }
        if tags.iter().any(|tag| tag.contains(term)) {
            score += 90;
        }
    }
    if terms.is_empty() { 0 } else { score }
}
