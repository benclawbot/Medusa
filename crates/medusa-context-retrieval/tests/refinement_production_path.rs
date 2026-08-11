use std::collections::BTreeSet;

use medusa_context::{
    ContextLedger,
    refinement::{
        ApprovalAuthority, ApprovalReceipt, EvaluationResult, EvidenceKind, EvidenceRef,
        ProposerMetadata, RefinementArtifactKind, RefinementContent, RefinementError,
        RefinementEvent, RefinementJournal, RefinementProposal, RefinementRisk, RefinementScope,
    },
};
use medusa_context_retrieval::{ContextRetriever, RetrievalQuery};
use time::macros::datetime;

struct TestAuthority;

impl ApprovalAuthority for TestAuthority {
    fn authorizes(&self, proposal: &RefinementProposal, receipt: &ApprovalReceipt) -> bool {
        receipt.approver == "user"
            && receipt.receipt_id == format!("approved:{}:{}", proposal.id, proposal.version)
    }
}

fn proposal(version: u64, value: &str) -> RefinementProposal {
    RefinementProposal {
        id: "testing".into(),
        version,
        artifact_kind: RefinementArtifactKind::RepositoryConvention,
        scope: RefinementScope::Repository,
        evidence: vec![EvidenceRef {
            id: "event-7".into(),
            kind: EvidenceKind::UserCorrection,
            trajectory_id: "trajectory-1".into(),
            start_sequence: 7,
            end_sequence: 7,
        }],
        before: None,
        after: RefinementContent::RepositoryConvention {
            key: "testing.workflow".into(),
            value: value.into(),
        },
        rationale: "user correction established a reusable convention".into(),
        expected_outcome: "future work follows the corrected workflow".into(),
        proposer: ProposerMetadata {
            model: "model-a".into(),
            route: "primary".into(),
            version: "1".into(),
        },
        risk: RefinementRisk::Low,
    }
}

fn evaluate_and_approve(journal: &mut RefinementJournal, version: u64) {
    let at = datetime!(2026-08-11 12:00 UTC);
    journal
        .append(
            RefinementEvent::Validated {
                proposal_id: "testing".into(),
                version,
            },
            at,
        )
        .unwrap();
    journal
        .append(
            RefinementEvent::Evaluated {
                proposal_id: "testing".into(),
                version,
                result: EvaluationResult {
                    evaluator: "deterministic-suite".into(),
                    validation_passed: true,
                    regression_passed: true,
                    effectiveness_passed: true,
                    notes: "passed".into(),
                },
            },
            at,
        )
        .unwrap();
    journal
        .append_approved(
            "testing",
            version,
            ApprovalReceipt {
                approver: "user".into(),
                receipt_id: format!("approved:testing:{version}"),
            },
            at,
            &TestAuthority,
        )
        .unwrap();
}

fn activate(journal: &mut RefinementJournal, candidate: RefinementProposal) {
    let at = datetime!(2026-08-11 12:00 UTC);
    let version = candidate.version;
    journal
        .append(
            RefinementEvent::Proposed {
                proposal: candidate,
            },
            at,
        )
        .unwrap();
    evaluate_and_approve(journal, version);
    journal
        .append(
            RefinementEvent::Activated {
                proposal_id: "testing".into(),
                version,
            },
            at,
        )
        .unwrap();
}

#[test]
fn artifact_kind_must_match_content_variant() {
    let mut candidate = proposal(1, "v1");
    candidate.artifact_kind = RefinementArtifactKind::Memory;
    assert_eq!(candidate.validate(), Err(RefinementError::InvalidProposal));
}

#[test]
fn approval_requires_authoritative_receipt_validation() {
    let at = datetime!(2026-08-11 12:00 UTC);
    let mut journal = RefinementJournal::default();
    journal
        .append(
            RefinementEvent::Proposed {
                proposal: proposal(1, "v1"),
            },
            at,
        )
        .unwrap();
    journal
        .append(
            RefinementEvent::Validated {
                proposal_id: "testing".into(),
                version: 1,
            },
            at,
        )
        .unwrap();
    journal
        .append(
            RefinementEvent::Evaluated {
                proposal_id: "testing".into(),
                version: 1,
                result: EvaluationResult {
                    evaluator: "suite".into(),
                    validation_passed: true,
                    regression_passed: true,
                    effectiveness_passed: true,
                    notes: "passed".into(),
                },
            },
            at,
        )
        .unwrap();
    assert_eq!(
        journal.append(
            RefinementEvent::Approved {
                proposal_id: "testing".into(),
                version: 1,
                receipt: ApprovalReceipt {
                    approver: "user".into(),
                    receipt_id: "made-up".into(),
                },
            },
            at,
        ),
        Err(RefinementError::ApprovalRequired)
    );
}

#[test]
fn serialized_approval_must_be_revalidated_by_authority() {
    let mut journal = RefinementJournal::default();
    activate(&mut journal, proposal(1, "v1"));
    let encoded = serde_json::to_vec(&journal).unwrap();
    let mut loaded: RefinementJournal = serde_json::from_slice(&encoded).unwrap();
    assert_eq!(loaded.projection(), Err(RefinementError::ApprovalRequired));
    loaded.revalidate_approvals(&TestAuthority).unwrap();
    assert_eq!(loaded.projection().unwrap().active()[0].version, 1);
}

#[test]
fn rollback_restores_only_the_direct_superseded_predecessor() {
    let at = datetime!(2026-08-11 12:00 UTC);
    let mut journal = RefinementJournal::default();
    activate(&mut journal, proposal(1, "v1"));

    let mut v2 = proposal(2, "v2");
    v2.before = Some(RefinementContent::RepositoryConvention {
        key: "testing.workflow".into(),
        value: "v1".into(),
    });
    journal
        .append(RefinementEvent::Proposed { proposal: v2 }, at)
        .unwrap();
    evaluate_and_approve(&mut journal, 2);
    journal
        .append(
            RefinementEvent::Superseded {
                proposal_id: "testing".into(),
                version: 1,
                by_proposal_id: "testing".into(),
                by_version: 2,
            },
            at,
        )
        .unwrap();
    journal
        .append(
            RefinementEvent::Activated {
                proposal_id: "testing".into(),
                version: 2,
            },
            at,
        )
        .unwrap();

    let mut v3 = proposal(3, "v3");
    v3.before = Some(RefinementContent::RepositoryConvention {
        key: "testing.workflow".into(),
        value: "v2".into(),
    });
    journal
        .append(RefinementEvent::Proposed { proposal: v3 }, at)
        .unwrap();
    evaluate_and_approve(&mut journal, 3);
    journal
        .append(
            RefinementEvent::Superseded {
                proposal_id: "testing".into(),
                version: 2,
                by_proposal_id: "testing".into(),
                by_version: 3,
            },
            at,
        )
        .unwrap();
    journal
        .append(
            RefinementEvent::Activated {
                proposal_id: "testing".into(),
                version: 3,
            },
            at,
        )
        .unwrap();

    assert_eq!(
        journal.append(
            RefinementEvent::RolledBack {
                proposal_id: "testing".into(),
                version: 3,
                restore_proposal_id: Some("testing".into()),
                restore_version: Some(1),
                reason: "regression".into(),
            },
            at,
        ),
        Err(RefinementError::InvalidTransition)
    );
}

#[test]
fn active_refinement_flows_through_context_retrieval() {
    let at = datetime!(2026-08-11 12:00 UTC);
    let mut journal = RefinementJournal::default();
    activate(
        &mut journal,
        proposal(1, "collect all CI failures first before making fixes"),
    );

    let mut ledger = ContextLedger::default();
    assert_eq!(journal.append_active_to_ledger(&mut ledger, at).unwrap(), 1);
    assert_eq!(journal.append_active_to_ledger(&mut ledger, at).unwrap(), 0);

    let result = ContextRetriever
        .retrieve(
            &ledger,
            &RetrievalQuery {
                text: "CI failures".into(),
                required_ids: BTreeSet::new(),
                preferred_kinds: BTreeSet::new(),
                maximum_items: 8,
                maximum_bytes: 4096,
            },
        )
        .unwrap();

    assert_eq!(result.ids(), vec!["refinement:testing:1"]);
    assert!(result.items[0].item.content.contains("evidence=[event-7]"));
}
