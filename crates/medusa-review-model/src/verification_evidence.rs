use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{ReviewSnapshot, VerificationState};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum VerificationOutcome {
    Passed,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DiagnosticSeverity {
    Info,
    Warning,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerificationDiagnostic {
    pub path: String,
    pub message: String,
    pub severity: DiagnosticSeverity,
    pub line: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerificationEvidence {
    pub id: String,
    pub check_name: String,
    pub outcome: VerificationOutcome,
    pub completed_at_unix_ms: i64,
    pub repository_fingerprint: String,
    pub affected_paths: Vec<String>,
    pub diagnostics: Vec<VerificationDiagnostic>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileVerificationSummary {
    pub path: String,
    pub state: VerificationState,
    pub evidence_ids: Vec<String>,
    pub unresolved_diagnostics: Vec<VerificationDiagnostic>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerificationReport {
    pub files: Vec<FileVerificationSummary>,
    pub unmatched_diagnostics: Vec<VerificationDiagnostic>,
}

#[derive(Debug, Clone, PartialEq, Eq, Error, Serialize, Deserialize)]
pub enum VerificationEvidenceError {
    #[error("verification evidence id must not be empty")]
    EmptyEvidenceId,
    #[error("verification evidence id is duplicated: {0}")]
    DuplicateEvidenceId(String),
    #[error("verification check name must not be empty")]
    EmptyCheckName,
    #[error("verification timestamp must not be negative")]
    InvalidTimestamp,
    #[error("verification repository fingerprint must not be empty")]
    EmptyRepositoryFingerprint,
    #[error("verification evidence contains duplicate affected path: {0}")]
    DuplicateAffectedPath(String),
    #[error("verification evidence references a path outside the review snapshot: {0}")]
    UnknownAffectedPath(String),
    #[error("verification diagnostic message must not be empty")]
    EmptyDiagnosticMessage,
}

pub fn associate_verification_evidence(
    snapshot: &mut ReviewSnapshot,
    evidence: &[VerificationEvidence],
) -> Result<VerificationReport, VerificationEvidenceError> {
    let known_paths = snapshot
        .files
        .iter()
        .map(|file| file.path.clone())
        .collect::<BTreeSet<_>>();
    let mut evidence_ids = BTreeSet::new();
    for item in evidence {
        if !evidence_ids.insert(item.id.clone()) {
            return Err(VerificationEvidenceError::DuplicateEvidenceId(
                item.id.clone(),
            ));
        }
        validate_evidence(item, &known_paths)?;
    }

    let mut by_path = BTreeMap::<String, Vec<&VerificationEvidence>>::new();
    let mut unmatched_diagnostics = Vec::new();
    for item in evidence {
        for path in &item.affected_paths {
            by_path.entry(path.clone()).or_default().push(item);
        }
        for diagnostic in &item.diagnostics {
            if known_paths.contains(&diagnostic.path) {
                by_path
                    .entry(diagnostic.path.clone())
                    .or_default()
                    .push(item);
            } else {
                unmatched_diagnostics.push(diagnostic.clone());
            }
        }
    }

    let mut files = Vec::with_capacity(snapshot.files.len());
    for file in &mut snapshot.files {
        let related = by_path.remove(&file.path).unwrap_or_default();
        let latest_by_check = latest_evidence_by_check(related);
        let repository_is_current = latest_by_check
            .values()
            .any(|item| item.repository_fingerprint == snapshot.repository_fingerprint);
        let unresolved_diagnostics = latest_by_check
            .values()
            .flat_map(|item| item.diagnostics.iter())
            .filter(|diagnostic| diagnostic.path == file.path)
            .filter(|diagnostic| diagnostic.severity == DiagnosticSeverity::Error)
            .cloned()
            .collect::<Vec<_>>();
        let has_failed = latest_by_check.values().any(|item| {
            item.outcome == VerificationOutcome::Failed
                || item.diagnostics.iter().any(|diagnostic| {
                    diagnostic.path == file.path && diagnostic.severity == DiagnosticSeverity::Error
                })
        });
        let has_stale = !latest_by_check.is_empty() && !repository_is_current;

        file.verification = if has_failed {
            VerificationState::Failed
        } else if has_stale {
            VerificationState::Stale
        } else if repository_is_current {
            VerificationState::Verified
        } else {
            VerificationState::Unverified
        };

        let evidence_ids = latest_by_check
            .values()
            .map(|item| item.id.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        file.provenance.verification_event_ids = evidence_ids.clone();

        files.push(FileVerificationSummary {
            path: file.path.clone(),
            state: file.verification,
            evidence_ids,
            unresolved_diagnostics,
        });
    }

    Ok(VerificationReport {
        files,
        unmatched_diagnostics,
    })
}

fn latest_evidence_by_check(
    mut related: Vec<&VerificationEvidence>,
) -> BTreeMap<String, &VerificationEvidence> {
    related.sort_by(|left, right| {
        left.completed_at_unix_ms
            .cmp(&right.completed_at_unix_ms)
            .then_with(|| left.id.cmp(&right.id))
    });
    let mut latest = BTreeMap::new();
    for item in related {
        latest.insert(item.check_name.clone(), item);
    }
    latest
}

fn validate_evidence(
    evidence: &VerificationEvidence,
    known_paths: &BTreeSet<String>,
) -> Result<(), VerificationEvidenceError> {
    if evidence.id.trim().is_empty() {
        return Err(VerificationEvidenceError::EmptyEvidenceId);
    }
    if evidence.check_name.trim().is_empty() {
        return Err(VerificationEvidenceError::EmptyCheckName);
    }
    if evidence.completed_at_unix_ms < 0 {
        return Err(VerificationEvidenceError::InvalidTimestamp);
    }
    if evidence.repository_fingerprint.trim().is_empty() {
        return Err(VerificationEvidenceError::EmptyRepositoryFingerprint);
    }

    let mut paths = BTreeSet::new();
    for path in &evidence.affected_paths {
        if !paths.insert(path.clone()) {
            return Err(VerificationEvidenceError::DuplicateAffectedPath(
                path.clone(),
            ));
        }
        if !known_paths.contains(path) {
            return Err(VerificationEvidenceError::UnknownAffectedPath(path.clone()));
        }
    }
    for diagnostic in &evidence.diagnostics {
        if diagnostic.message.trim().is_empty() {
            return Err(VerificationEvidenceError::EmptyDiagnosticMessage);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::{
        ChangeKind, ChangeOrigin, ReviewFile, ReviewProvenance, ReviewSnapshot, ReviewState,
    };

    use super::*;

    fn review_file(path: &str) -> ReviewFile {
        ReviewFile {
            path: path.into(),
            previous_path: None,
            kind: ChangeKind::Modified,
            origin: ChangeOrigin::Medusa,
            binary: false,
            policy_sensitive: false,
            verification: VerificationState::Unverified,
            review_state: ReviewState::Unreviewed,
            snapshot_fingerprint: format!("{path}-v1"),
            current_fingerprint: format!("{path}-v2"),
            hunks: vec![],
            provenance: ReviewProvenance {
                task_step_id: Some("step-1".into()),
                tool_execution_id: None,
                rationale: None,
                verification_event_ids: vec![],
            },
        }
    }

    fn snapshot() -> ReviewSnapshot {
        ReviewSnapshot {
            id: "snapshot-1".into(),
            repository_fingerprint: "repo-v2".into(),
            created_at_unix_ms: 1,
            files: vec![review_file("src/lib.rs")],
        }
    }

    fn evidence(outcome: VerificationOutcome, fingerprint: &str) -> VerificationEvidence {
        VerificationEvidence {
            id: "verify-1".into(),
            check_name: "cargo test".into(),
            outcome,
            completed_at_unix_ms: 10,
            repository_fingerprint: fingerprint.into(),
            affected_paths: vec!["src/lib.rs".into()],
            diagnostics: vec![],
        }
    }

    #[test]
    fn marks_file_verified_when_evidence_matches_latest_repository() {
        let mut snapshot = snapshot();
        let report = associate_verification_evidence(
            &mut snapshot,
            &[evidence(VerificationOutcome::Passed, "repo-v2")],
        )
        .unwrap();
        assert_eq!(snapshot.files[0].verification, VerificationState::Verified);
        assert_eq!(
            snapshot.files[0].provenance.verification_event_ids,
            vec!["verify-1"]
        );
        assert_eq!(report.files[0].state, VerificationState::Verified);
    }

    #[test]
    fn repository_level_evidence_verifies_multiple_affected_files() {
        let mut snapshot = snapshot();
        snapshot.files.push(review_file("src/main.rs"));
        let mut item = evidence(VerificationOutcome::Passed, "repo-v2");
        item.affected_paths.push("src/main.rs".into());
        let report = associate_verification_evidence(&mut snapshot, &[item]).unwrap();
        assert!(
            report
                .files
                .iter()
                .all(|file| file.state == VerificationState::Verified)
        );
    }

    #[test]
    fn marks_file_stale_after_later_edit_invalidates_evidence() {
        let mut snapshot = snapshot();
        associate_verification_evidence(
            &mut snapshot,
            &[evidence(VerificationOutcome::Passed, "repo-v1")],
        )
        .unwrap();
        assert_eq!(snapshot.files[0].verification, VerificationState::Stale);
        assert!(!snapshot.completion_state().verification_current);
    }

    #[test]
    fn successful_rerun_supersedes_earlier_failure_for_the_same_check() {
        let mut snapshot = snapshot();
        let mut failed = evidence(VerificationOutcome::Failed, "repo-v2");
        failed.id = "verify-failed".into();
        let mut passed = evidence(VerificationOutcome::Passed, "repo-v2");
        passed.id = "verify-passed".into();
        passed.completed_at_unix_ms = 20;
        let report = associate_verification_evidence(&mut snapshot, &[failed, passed]).unwrap();
        assert_eq!(report.files[0].state, VerificationState::Verified);
        assert_eq!(report.files[0].evidence_ids, vec!["verify-passed"]);
    }

    #[test]
    fn failed_check_and_diagnostic_block_verified_completion() {
        let mut snapshot = snapshot();
        let mut failed = evidence(VerificationOutcome::Failed, "repo-v2");
        failed.diagnostics.push(VerificationDiagnostic {
            path: "src/lib.rs".into(),
            message: "type mismatch".into(),
            severity: DiagnosticSeverity::Error,
            line: Some(12),
        });
        let report = associate_verification_evidence(&mut snapshot, &[failed]).unwrap();
        assert_eq!(snapshot.files[0].verification, VerificationState::Failed);
        assert_eq!(report.files[0].unresolved_diagnostics.len(), 1);
        assert!(!snapshot.completion_state().verification_current);
    }

    #[test]
    fn error_diagnostic_fails_file_even_when_check_outcome_is_passed() {
        let mut snapshot = snapshot();
        let mut contradictory = evidence(VerificationOutcome::Passed, "repo-v2");
        contradictory.diagnostics.push(VerificationDiagnostic {
            path: "src/lib.rs".into(),
            message: "unresolved compiler error".into(),
            severity: DiagnosticSeverity::Error,
            line: None,
        });
        let report = associate_verification_evidence(&mut snapshot, &[contradictory]).unwrap();
        assert_eq!(report.files[0].state, VerificationState::Failed);
        assert!(!snapshot.completion_state().verification_current);
    }

    #[test]
    fn rejects_duplicate_evidence_ids_across_the_batch() {
        let mut snapshot = snapshot();
        let first = evidence(VerificationOutcome::Failed, "repo-v2");
        let mut second = evidence(VerificationOutcome::Passed, "repo-v2");
        second.completed_at_unix_ms = 20;
        assert_eq!(
            associate_verification_evidence(&mut snapshot, &[first, second]),
            Err(VerificationEvidenceError::DuplicateEvidenceId(
                "verify-1".into()
            ))
        );
    }

    #[test]
    fn rejects_unknown_affected_paths_but_surfaces_unmatched_diagnostics() {
        let mut snapshot = snapshot();
        let mut invalid = evidence(VerificationOutcome::Passed, "repo-v2");
        invalid.affected_paths = vec!["other.rs".into()];
        assert_eq!(
            associate_verification_evidence(&mut snapshot, &[invalid]),
            Err(VerificationEvidenceError::UnknownAffectedPath(
                "other.rs".into()
            ))
        );

        let mut valid = evidence(VerificationOutcome::Passed, "repo-v2");
        valid.diagnostics.push(VerificationDiagnostic {
            path: "workspace".into(),
            message: "global failure".into(),
            severity: DiagnosticSeverity::Error,
            line: None,
        });
        let report = associate_verification_evidence(&mut snapshot, &[valid]).unwrap();
        assert_eq!(report.unmatched_diagnostics.len(), 1);
    }
}
