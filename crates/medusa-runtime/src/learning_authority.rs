//! Runtime boundary for the canonical refinement authority.

use std::path::Path;

use medusa_context::refinement::{
    EvaluationResult, EvidenceKind, EvidenceRef, ProposerMetadata, RefinementArtifactKind,
    RefinementContent, RefinementProposal, RefinementRisk, RefinementScope,
};
use medusa_improvement::{
    learning_admission::LearningAdmissionPolicy,
    refinement_authority::{
        ApprovalActorClass, RefinementAuthoritySnapshot, RefinementAuthorityStore,
    },
    refinement_migration::RefinementMigrator,
};
use sha2::{Digest, Sha256};

pub(crate) fn open(repo: &Path) -> Result<RefinementAuthorityStore, String> {
    let mut store = RefinementAuthorityStore::open(repo).map_err(|error| error.to_string())?;
    let policy = LearningAdmissionPolicy::for_repository(repo).map_err(|error| error.to_string())?;
    if policy.capture_enabled() {
        RefinementMigrator::run(repo, &mut store).map_err(|error| error.to_string())?;
    }
    Ok(store)
}

pub(crate) fn privacy(
    store: &RefinementAuthorityStore,
) -> Result<medusa_core::learning_policy::LearningPrivacyPolicy, String> {
    store.privacy().map_err(|error| error.to_string())
}

pub(crate) fn privacy_revision(store: &RefinementAuthorityStore) -> Result<u64, String> {
    store.privacy_revision().map_err(|error| error.to_string())
}

pub(crate) fn update_privacy(
    store: &RefinementAuthorityStore,
    privacy: medusa_core::learning_policy::LearningPrivacyPolicy,
    expected_revision: u64,
) -> Result<u64, String> {
    store
        .update_privacy(privacy, expected_revision)
        .map_err(|error| error.to_string())
}

pub(crate) fn resolve(
    store: &RefinementAuthorityStore,
    requested_id: &str,
) -> Result<(String, u64), String> {
    let snapshot = store.snapshot().map_err(|error| error.to_string())?;
    if let Some(record) = snapshot
        .records
        .iter()
        .find(|record| record.proposal_id == requested_id)
    {
        return Ok((record.proposal_id.clone(), record.version));
    }
    let path = store.root().join("migrations.jsonl");
    if let Ok(text) = std::fs::read_to_string(path) {
        for line in text.lines().filter(|line| !line.trim().is_empty()) {
            let value: serde_json::Value = serde_json::from_str(line).map_err(|error| error.to_string())?;
            if value["source_record_id"].as_str() == Some(requested_id) {
                if let (Some(id), Some(version)) = (
                    value["canonical_proposal_id"].as_str(),
                    value["canonical_version"].as_u64(),
                ) {
                    return Ok((id.to_owned(), version));
                }
            }
        }
    }
    Err(format!("canonical refinement {requested_id} was not found"))
}

pub(crate) fn propose(
    store: &mut RefinementAuthorityStore,
    scope: &str,
    key: &str,
    value: &str,
) -> Result<RefinementAuthoritySnapshot, String> {
    let scope = parse_scope(scope)?;
    if key.trim().is_empty() || value.trim().is_empty() {
        return Err("/learning propose requires a non-empty key and value".into());
    }
    let digest = hex::encode(Sha256::digest(format!("{scope:?}\n{key}\n{value}").as_bytes()));
    let proposal = RefinementProposal {
        id: format!("runtime-{digest}"),
        version: 1,
        artifact_kind: RefinementArtifactKind::RepositoryConvention,
        scope,
        evidence: vec![EvidenceRef {
            id: format!("runtime-correction-{digest}"),
            kind: EvidenceKind::UserCorrection,
            trajectory_id: "runtime-command".into(),
            start_sequence: 1,
            end_sequence: 1,
        }],
        before: None,
        after: RefinementContent::RepositoryConvention {
            key: key.to_owned(),
            value: value.to_owned(),
        },
        rationale: "explicit user correction entered through the canonical runtime command".into(),
        expected_outcome: "matching future turns use the corrected behavior".into(),
        proposer: ProposerMetadata {
            model: "runtime-user".into(),
            route: "learning-command".into(),
            version: "1".into(),
        },
        risk: RefinementRisk::Low,
    };
    let revision = store.snapshot().map_err(|error| error.to_string())?.revision;
    store
        .propose(proposal, revision)
        .map_err(|error| error.to_string())
}

pub(crate) fn evaluate(
    store: &mut RefinementAuthorityStore,
    requested_id: &str,
    validation_passed: bool,
    regression_passed: bool,
    effectiveness_passed: bool,
) -> Result<RefinementAuthoritySnapshot, String> {
    let (id, version) = resolve(store, requested_id)?;
    let revision = store.snapshot().map_err(|error| error.to_string())?.revision;
    store
        .record_evaluation(
            &id,
            version,
            EvaluationResult {
                evaluator: "runtime-command".into(),
                validation_passed,
                regression_passed,
                effectiveness_passed,
                notes: format!(
                    "manual evaluation: validation={} regression={} effectiveness={}",
                    pass_label(validation_passed),
                    pass_label(regression_passed),
                    pass_label(effectiveness_passed)
                ),
            },
            revision,
        )
        .map_err(|error| error.to_string())
}

pub(crate) fn transition(
    store: &mut RefinementAuthorityStore,
    requested_id: &str,
    action: RuntimeLearningAction,
) -> Result<RefinementAuthoritySnapshot, String> {
    let (id, version) = resolve(store, requested_id)?;
    let revision = store.snapshot().map_err(|error| error.to_string())?.revision;
    let result = match action {
        RuntimeLearningAction::Approve => store.approve(
            &id,
            version,
            ApprovalActorClass::User,
            &format!("runtime-approval-{id}-{version}"),
            now_unix_ms(),
            revision,
        ),
        RuntimeLearningAction::Reject => store.reject(&id, version, "rejected by runtime user", revision),
        RuntimeLearningAction::Defer => store.defer(&id, version, "deferred by runtime user", revision),
        RuntimeLearningAction::Validate => store.validate(&id, version, revision),
        RuntimeLearningAction::Activate => store.activate(&id, version, revision),
        RuntimeLearningAction::Suspend => store.suspend(&id, version, "suspended by runtime user", revision),
        RuntimeLearningAction::Rollback => store.rollback(
            &id,
            version,
            None,
            None,
            "rolled back by runtime user",
            revision,
        ),
        RuntimeLearningAction::Delete => store.tombstone(&id, version, "deleted by runtime user", revision),
    };
    result.map_err(|error| error.to_string())
}

pub(crate) fn inspect(
    store: &RefinementAuthorityStore,
    requested_id: &str,
) -> Result<Vec<String>, String> {
    let (id, version) = resolve(store, requested_id)?;
    let snapshot = store.snapshot().map_err(|error| error.to_string())?;
    let record = snapshot
        .records
        .iter()
        .find(|record| record.proposal_id == id && record.version == version)
        .ok_or_else(|| format!("canonical refinement {requested_id} was not found"))?;
    Ok(vec![
        format!("id={} version={} lifecycle={:?}", id, version, record.lifecycle),
        format!("scope={:?} artifact={:?}", record.scope, record.artifact_kind),
        format!("evidence_digest={}", record.evidence_digest),
        format!("approval_receipt={}", record.approval_receipt_id.as_deref().unwrap_or("none")),
        format!("journal_head={}", snapshot.journal_head_hash),
    ])
}

pub(crate) fn details(snapshot: &RefinementAuthoritySnapshot, filter: Option<&str>) -> Vec<String> {
    let mut details = vec![format!(
        "canonical authority revision={} journal_head={}",
        snapshot.revision, snapshot.journal_head_hash
    )];
    for record in &snapshot.records {
        let proposal_text = record.proposal.as_ref().map_or_else(String::new, |proposal| {
            format!(" {:?}", proposal.after)
        });
        let searchable = format!(
            "{} {:?} {:?} {:?}{proposal_text}",
            record.proposal_id, record.lifecycle, record.scope, record.artifact_kind
        )
        .to_ascii_lowercase();
        if filter.is_some_and(|value| !searchable.contains(&value.to_ascii_lowercase())) {
            continue;
        }
        details.push(format!(
            "{} v{} | {:?} | {:?} | {:?} | approval={}",
            record.proposal_id,
            record.version,
            record.lifecycle,
            record.scope,
            record.artifact_kind,
            record.approval_receipt_id.as_deref().unwrap_or("none")
        ));
    }
    if snapshot.records.is_empty() {
        details.push("No canonical refinement proposals are recorded.".into());
    }
    details
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum RuntimeLearningAction {
    Approve,
    Reject,
    Defer,
    Validate,
    Activate,
    Suspend,
    Rollback,
    Delete,
}

fn pass_label(passed: bool) -> &'static str {
    if passed { "pass" } else { "fail" }
}

fn parse_scope(value: &str) -> Result<RefinementScope, String> {
    match value.to_ascii_lowercase().as_str() {
        "repository" => Ok(RefinementScope::Repository),
        "user" => Ok(RefinementScope::User),
        "session" => Ok(RefinementScope::Session),
        _ => Err("/learning propose scope must be repository, user, or session".into()),
    }
}

fn now_unix_ms() -> i64 {
    let nanos = time::OffsetDateTime::now_utc().unix_timestamp_nanos();
    i64::try_from(nanos / 1_000_000).unwrap_or(i64::MAX)
}
