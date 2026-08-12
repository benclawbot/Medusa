//! Runtime-facing compatibility projection over the canonical refinement authority.

use std::path::Path;

use medusa_core::learning_policy::LearningPrivacyPolicy;
pub use medusa_improvement::learning_review::{
    LearningAuditExport, LearningPrivacy, LearningReviewError, LearningReviewItem,
    LearningReviewSnapshot, LearningReviewState, LearningKind, RedactionPreview,
};
use medusa_improvement::refinement_authority::RefinementAuthoritySnapshot;

pub fn read(repo: &Path) -> Result<LearningReviewSnapshot, LearningReviewError> {
    let store = crate::learning_authority::open(repo).map_err(LearningReviewError::Canonical)?;
    let snapshot = store
        .snapshot()
        .map_err(|error| LearningReviewError::Canonical(error.to_string()))?;
    let privacy = crate::learning_authority::privacy(&store)
        .map_err(LearningReviewError::Canonical)?;
    Ok(project_snapshot(&snapshot, privacy))
}

pub fn transition(
    repo: &Path,
    item_id: &str,
    target: LearningReviewState,
    expected_revision: u64,
    actor: &str,
) -> Result<LearningReviewSnapshot, LearningReviewError> {
    let mut store = crate::learning_authority::open(repo).map_err(LearningReviewError::Canonical)?;
    let action = match target {
        LearningReviewState::Approved => crate::learning_authority::RuntimeLearningAction::Approve,
        LearningReviewState::Rejected => crate::learning_authority::RuntimeLearningAction::Reject,
        LearningReviewState::Deferred => crate::learning_authority::RuntimeLearningAction::Defer,
        LearningReviewState::Validated => crate::learning_authority::RuntimeLearningAction::Validate,
        LearningReviewState::Active => crate::learning_authority::RuntimeLearningAction::Activate,
        LearningReviewState::Suspended => crate::learning_authority::RuntimeLearningAction::Suspend,
        LearningReviewState::RolledBack => crate::learning_authority::RuntimeLearningAction::Rollback,
        LearningReviewState::Deleted => crate::learning_authority::RuntimeLearningAction::Delete,
        LearningReviewState::Proposed | LearningReviewState::Conflict => {
            return Err(LearningReviewError::Canonical(
                "the requested learning state is not a runtime transition".into(),
            ));
        }
    };
    let actual = store
        .snapshot()
        .map_err(|error| LearningReviewError::Canonical(error.to_string()))?
        .revision;
    if actual != expected_revision {
        return Err(LearningReviewError::Conflict {
            expected: expected_revision,
            actual,
        });
    }
    let _ = actor;
    crate::learning_authority::transition(&mut store, item_id, action)
        .map_err(LearningReviewError::Canonical)?;
    read(repo)
}

pub fn propose(
    repo: &Path,
    scope: &str,
    key: &str,
    value: &str,
) -> Result<LearningReviewSnapshot, LearningReviewError> {
    let mut store = crate::learning_authority::open(repo).map_err(LearningReviewError::Canonical)?;
    crate::learning_authority::propose(&mut store, scope, key, value)
        .map_err(LearningReviewError::Canonical)?;
    read(repo)
}

pub fn evaluate(
    repo: &Path,
    id: &str,
    passed: bool,
) -> Result<LearningReviewSnapshot, LearningReviewError> {
    let mut store = crate::learning_authority::open(repo).map_err(LearningReviewError::Canonical)?;
    crate::learning_authority::evaluate(&mut store, id, passed)
        .map_err(LearningReviewError::Canonical)?;
    read(repo)
}

pub fn inspect(repo: &Path, id: &str) -> Result<Vec<String>, LearningReviewError> {
    let store = crate::learning_authority::open(repo).map_err(LearningReviewError::Canonical)?;
    crate::learning_authority::inspect(&store, id).map_err(LearningReviewError::Canonical)
}

pub fn update_privacy(
    repo: &Path,
    privacy: LearningPrivacy,
    expected_revision: u64,
    actor: &str,
) -> Result<LearningReviewSnapshot, LearningReviewError> {
    let store = crate::learning_authority::open(repo).map_err(LearningReviewError::Canonical)?;
    let policy = LearningPrivacyPolicy {
        capture_enabled: privacy.capture_enabled,
        user_persistence_enabled: privacy.user_persistence_enabled,
        cross_repository_reuse_enabled: privacy.cross_repository_reuse_enabled,
        telemetry_enabled: privacy.telemetry_enabled,
        automatic_proposals_enabled: privacy.automatic_proposals_enabled,
    };
    let actual = crate::learning_authority::privacy_revision(&store)
        .map_err(LearningReviewError::Canonical)?;
    let authority_revision = store
        .snapshot()
        .map_err(|error| LearningReviewError::Canonical(error.to_string()))?
        .revision;
    if actual != expected_revision && authority_revision != expected_revision {
        return Err(LearningReviewError::Conflict {
            expected: expected_revision,
            actual: authority_revision.max(actual),
        });
    }
    let _ = actor;
    crate::learning_authority::update_privacy(&store, policy, actual)
        .map_err(LearningReviewError::Canonical)?;
    read(repo)
}

pub fn redaction_preview(repo: &Path) -> Result<RedactionPreview, LearningReviewError> {
    let snapshot = read(repo)?;
    let mut blocked_fields = Vec::new();
    let mut warnings = Vec::new();
    for item in &snapshot.items {
        for (field, value) in [
            ("title", item.title.as_str()),
            ("root_cause", item.root_cause.as_str()),
            ("generalized_rule", item.generalized_rule.as_str()),
            ("proposed_solution", item.proposed_solution.as_str()),
        ] {
            if sensitive(value) {
                blocked_fields.push(format!("{}:{field}", item.id));
                warnings.push(format!("{} contains a secret-like marker", item.id));
            }
        }
    }
    blocked_fields.sort();
    blocked_fields.dedup();
    warnings.sort();
    warnings.dedup();
    Ok(RedactionPreview {
        safe: blocked_fields.is_empty(),
        blocked_fields,
        warnings,
        item_count: snapshot.items.len(),
    })
}

pub fn export(repo: &Path) -> Result<LearningAuditExport, LearningReviewError> {
    let snapshot = read(repo)?;
    let redaction = redaction_preview(repo)?;
    if !redaction.safe {
        return Err(LearningReviewError::SensitiveExportBlocked(
            redaction.blocked_fields.clone(),
        ));
    }
    Ok(LearningAuditExport {
        snapshot,
        events: Vec::new(),
        chain_valid: true,
        redaction,
    })
}

fn project_snapshot(
    canonical: &RefinementAuthoritySnapshot,
    privacy: LearningPrivacyPolicy,
) -> LearningReviewSnapshot {
    let items = canonical
        .records
        .iter()
        .filter_map(|record| {
            let proposal = record.proposal.as_ref()?;
            let (title, generalized_rule) = content_fields(proposal);
            Some(LearningReviewItem {
                id: record.proposal_id.clone(),
                revision: canonical.revision.max(1),
                state: project_state(record.lifecycle),
                kind: project_kind(record.artifact_kind),
                title: title.to_owned(),
                source_signal_ids: proposal
                    .evidence
                    .iter()
                    .map(|item| item.id.clone())
                    .collect(),
                evidence_digests: vec![record.evidence_digest.clone()],
                root_cause: proposal.rationale.clone(),
                generalized_rule: generalized_rule.to_owned(),
                scope: format!("{:?}", record.scope).to_ascii_lowercase(),
                confidence_milli: 0,
                proposed_solution: generalized_rule.to_owned(),
                non_applicable_contexts: Vec::new(),
                replay: None,
                conflicts_with: canonical.conflict_keys.iter().cloned().collect(),
                active_version: (record.lifecycle
                    == medusa_context::refinement::RefinementLifecycle::Active)
                    .then(|| format!("{}-v{}", record.proposal_id, record.version)),
                previous_version: record.predecessor_proposal_id.clone(),
                created_at_unix_ms: proposal_timestamp_ms(record),
                updated_at_unix_ms: proposal_timestamp_ms(record),
            })
        })
        .collect();
    LearningReviewSnapshot {
        schema_version: 1,
        revision: canonical.revision,
        privacy: LearningPrivacy {
            capture_enabled: privacy.capture_enabled,
            user_persistence_enabled: privacy.user_persistence_enabled,
            cross_repository_reuse_enabled: privacy.cross_repository_reuse_enabled,
            telemetry_enabled: privacy.telemetry_enabled,
            automatic_proposals_enabled: privacy.automatic_proposals_enabled,
        },
        items,
        audit_head: canonical.journal_head_hash.clone(),
    }
}

fn content_fields(proposal: &medusa_context::refinement::RefinementProposal) -> (&str, &str) {
    match &proposal.after {
        medusa_context::refinement::RefinementContent::Memory { key, value }
        | medusa_context::refinement::RefinementContent::RepositoryConvention { key, value }
        | medusa_context::refinement::RefinementContent::PromptGuidance {
            key,
            guidance: value,
        } => (key, value),
        medusa_context::refinement::RefinementContent::WorkflowMetadata { name, summary }
        | medusa_context::refinement::RefinementContent::TeamRoleMetadata {
            name,
            guidance: summary,
        } => (name, summary),
    }
}

fn project_state(
    lifecycle: medusa_context::refinement::RefinementLifecycle,
) -> LearningReviewState {
    match lifecycle {
        medusa_context::refinement::RefinementLifecycle::Proposed => LearningReviewState::Proposed,
        medusa_context::refinement::RefinementLifecycle::Deferred => LearningReviewState::Deferred,
        medusa_context::refinement::RefinementLifecycle::Validated => LearningReviewState::Validated,
        medusa_context::refinement::RefinementLifecycle::Evaluated
        | medusa_context::refinement::RefinementLifecycle::Approved => LearningReviewState::Approved,
        medusa_context::refinement::RefinementLifecycle::Active => LearningReviewState::Active,
        medusa_context::refinement::RefinementLifecycle::Superseded => {
            LearningReviewState::RolledBack
        }
        medusa_context::refinement::RefinementLifecycle::Suspended => LearningReviewState::Suspended,
        medusa_context::refinement::RefinementLifecycle::RolledBack => LearningReviewState::RolledBack,
        medusa_context::refinement::RefinementLifecycle::Rejected => LearningReviewState::Rejected,
        medusa_context::refinement::RefinementLifecycle::Tombstoned => LearningReviewState::Deleted,
        medusa_context::refinement::RefinementLifecycle::Conflict => LearningReviewState::Conflict,
    }
}

fn project_kind(
    kind: medusa_context::refinement::RefinementArtifactKind,
) -> medusa_improvement::learning_review::LearningKind {
    match kind {
        medusa_context::refinement::RefinementArtifactKind::Memory => LearningKind::SessionFact,
        medusa_context::refinement::RefinementArtifactKind::RepositoryConvention => {
            LearningKind::RepositoryLearning
        }
        medusa_context::refinement::RefinementArtifactKind::WorkflowMetadata => LearningKind::Policy,
        medusa_context::refinement::RefinementArtifactKind::TeamRoleMetadata => {
            LearningKind::UserPreference
        }
        medusa_context::refinement::RefinementArtifactKind::PromptGuidance => {
            LearningKind::ProductCodeChange
        }
    }
}

fn proposal_timestamp_ms(record: &medusa_context::refinement::RefinementRecord) -> i64 {
    let millis = record.last_recorded_at.unix_timestamp_nanos() / 1_000_000;
    i64::try_from(millis).unwrap_or(if millis.is_negative() {
        i64::MIN
    } else {
        i64::MAX
    })
}

fn sensitive(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    [
        "api_key",
        "apikey",
        "authorization:",
        "bearer ",
        "secret=",
        "token=",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absent_store_returns_private_empty_snapshot() {
        let repo = tempfile::tempdir().expect("repo");
        let snapshot = read(repo.path()).expect("snapshot");
        assert!(snapshot.items.is_empty());
        assert!(snapshot.privacy.capture_enabled);
        assert!(!snapshot.privacy.cross_repository_reuse_enabled);
        assert!(!snapshot.privacy.telemetry_enabled);
    }
}
