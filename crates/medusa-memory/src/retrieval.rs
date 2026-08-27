use std::cmp::Reverse;

use medusa_core::MedusaResult;
use time::OffsetDateTime;
use tracing::info;

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
        info!(matches = results.len(), "memory retrieval completed");
        Ok(results)
    }
}

fn score(document: &MemoryDocument, terms: &[String]) -> i64 {
    if terms.is_empty() {
        return 0;
    }

    let title = normalize(&document.title);
    let body = normalize(&document.body);
    let tags = document
        .tags
        .iter()
        .map(|tag| normalize(tag))
        .collect::<Vec<_>>();
    let mut match_score = 0_i64;
    for term in terms {
        if title.contains(term) {
            match_score += 120;
        }
        if body.contains(term) {
            match_score += 60;
        }
        if tags.iter().any(|tag| tag.contains(term)) {
            match_score += 90;
        }
    }
    if match_score == 0 {
        return 0;
    }

    let mut score = match_score + i64::from(document.confidence_milli) / 10;
    score += i64::from(document.successful_reuse_count) * 25;
    score += match document.validation {
        Validation::TestVerified => 80,
        Validation::UserStated | Validation::SourceVerified => 70,
        Validation::Observed => 60,
        _ => -500,
    };
    score
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::MemoryProposal;

    #[test]
    fn unrelated_high_confidence_memory_is_not_a_search_hit() {
        let directory = tempfile::tempdir().expect("tempdir");
        let engine = MemoryEngine::new(directory.path()).expect("engine");
        engine
            .commit_proposal(&MemoryProposal {
                memory_type: "command".into(),
                title: "Build command".into(),
                claim: "Run cargo build from the repository root.".into(),
                evidence: vec!["artifact://sessions/search/verification".into()],
                confidence_milli: 950,
                validation: Validation::TestVerified,
                scope: Scope::Project,
                project_id: Some("sha256:search-test".into()),
                session_id: Some("ses-search".into()),
                tags: vec!["rust".into()],
            })
            .expect("commit");

        assert!(
            engine
                .search("unrelated deletion marker", Scope::Project, 10)
                .expect("search")
                .is_empty()
        );
    }
}
