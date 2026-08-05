//! Durable review-before-integration transaction for every production mutation.

use std::{
    fs,
    path::{Path, PathBuf},
    sync::mpsc::Sender,
};

use medusa_agent::{AgentSession, authoritative_verification_for_components_at};
use medusa_evidence::{ChangedComponent, VerificationReceipt, changed_scope_fingerprint};
use medusa_provider::{MessageBlock, Role};
use medusa_review_model::{
    PARENT_REVIEW_RESPONSE_REQUIREMENT, ParentReviewOutcome, ParentReviewResponse,
    ParentReviewResponseError, final_parent_review_line, validate_parent_review_response,
};
pub use medusa_review_model::ParentReviewDecision;
use medusa_workers::{IntegrationReceipt, Worker, WorkerManager};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{RuntimeActivity, RuntimeActivityKind, RuntimeEvent};

const TRANSACTION_SCHEMA_VERSION: u16 = 1;
const MAX_REVISIONS: u32 = 2;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MutationLifecycle {
    Planned,
    PreparedInIsolation,
    ReviewPending,
    RevisionRequested,
    ReviewAccepted,
    VerificationPending,
    Verified,
    IntegrationAuthorized,
    Integrated,
    Reconciled,
    Cancelled,
    Failed,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ParentReviewReceipt {
    pub decision: ParentReviewDecision,
    pub reviewer_session_id: String,
    pub commit: String,
    pub tree: String,
    pub patch_fingerprint: String,
    pub scope_fingerprint: String,
    pub evidence_fingerprint: String,
    pub rationale: String,
    pub fingerprint: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct IndependentVerificationReceipt {
    pub verifier: String,
    pub commit: String,
    pub tree: String,
    pub changed_paths: Vec<String>,
    pub changed_components: Vec<ChangedComponent>,
    pub evidence: Vec<String>,
    pub authority: VerificationReceipt,
    pub fingerprint: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct IntegrationAuthorization {
    pub commit: String,
    pub base_head: String,
    pub primary_head: String,
    pub review_fingerprint: String,
    pub verification_fingerprint: String,
    pub fingerprint: String,
}

#[derive(Clone, Debug)]
pub struct PreparedMutationInput {
    pub plan_fingerprint: String,
    pub repository_fingerprint: String,
    pub task_id: String,
    pub base_head: String,
    pub worker: Worker,
    pub changed_paths: Vec<String>,
    pub changed_components: Vec<ChangedComponent>,
    pub implementation_summary: String,
    pub worktree_verification_evidence: Vec<String>,
    pub worktree_verification_receipt: VerificationReceipt,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct MutationTransactionSnapshot {
    pub schema_version: u16,
    pub transaction_id: String,
    pub lifecycle: MutationLifecycle,
    pub plan_fingerprint: String,
    pub repository_fingerprint: String,
    pub task_id: String,
    pub base_head: String,
    pub worker: Worker,
    pub prepared_commit: String,
    pub prepared_tree: String,
    pub changed_paths: Vec<String>,
    pub changed_components: Vec<ChangedComponent>,
    pub patch_fingerprint: String,
    pub scope_fingerprint: String,
    pub implementation_summary: String,
    pub implementation_evidence_fingerprint: String,
    pub worktree_verification_evidence: Vec<String>,
    pub worktree_verification_receipt: Option<VerificationReceipt>,
    pub review: Option<ParentReviewReceipt>,
    pub verification: Option<IndependentVerificationReceipt>,
    pub authorization: Option<IntegrationAuthorization>,
    pub integration: Option<IntegrationReceipt>,
    pub revision_count: u32,
    pub revision: u64,
    pub last_error: Option<String>,
    pub fingerprint: String,
}

pub enum TransactionCompletion {
    RevisionRequested(String),
    Reconciled(IntegrationReceipt),
}

pub struct MutationTransaction {
    root: PathBuf,
    path: PathBuf,
    state: MutationTransactionSnapshot,
}

impl MutationTransaction {
    pub fn open(path: impl Into<PathBuf>) -> Result<Self, String> {
        let path = path.into();
        let root = path
            .parent()
            .ok_or_else(|| "mutation transaction path has no parent".to_owned())?
            .to_path_buf();
        let state: MutationTransactionSnapshot =
            serde_json::from_slice(&fs::read(&path).map_err(|error| error.to_string())?)
                .map_err(|error| error.to_string())?;
        let transaction = Self { root, path, state };
        transaction.validate()?;
        Ok(transaction)
    }

    pub fn open_or_prepare(
        root: &Path,
        repo: &Path,
        input: PreparedMutationInput,
    ) -> Result<Self, String> {
        fs::create_dir_all(root).map_err(|error| error.to_string())?;
        let path = root.join("mutation-transaction.json");
        let manager = WorkerManager::new(repo, root.join("worktrees"))
            .map_err(|error| error.to_string())?;
        if path.is_file() {
            let mut transaction = Self::open(&path)?;
            transaction.validate_identity(&input)?;
            if matches!(
                transaction.state.lifecycle,
                MutationLifecycle::Planned | MutationLifecycle::RevisionRequested
            ) {
                transaction.prepare(&manager, input)?;
            }
            return Ok(transaction);
        }
        let transaction_id = hash(&(
            &input.plan_fingerprint,
            &input.repository_fingerprint,
            &input.task_id,
            &input.base_head,
            &input.worker.id,
        ));
        let mut transaction = Self {
            root: root.to_path_buf(),
            path,
            state: MutationTransactionSnapshot {
                schema_version: TRANSACTION_SCHEMA_VERSION,
                transaction_id,
                lifecycle: MutationLifecycle::Planned,
                plan_fingerprint: input.plan_fingerprint.clone(),
                repository_fingerprint: input.repository_fingerprint.clone(),
                task_id: input.task_id.clone(),
                base_head: input.base_head.clone(),
                worker: input.worker.clone(),
                prepared_commit: String::new(),
                prepared_tree: String::new(),
                changed_paths: Vec::new(),
                changed_components: Vec::new(),
                patch_fingerprint: String::new(),
                scope_fingerprint: String::new(),
                implementation_summary: String::new(),
                implementation_evidence_fingerprint: String::new(),
                worktree_verification_evidence: Vec::new(),
                worktree_verification_receipt: None,
                review: None,
                verification: None,
                authorization: None,
                integration: None,
                revision_count: 0,
                revision: 0,
                last_error: None,
                fingerprint: String::new(),
            },
        };
        transaction.refresh();
        transaction.persist()?;
        transaction.prepare(&manager, input)?;
        Ok(transaction)
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    #[must_use]
    pub fn snapshot(&self) -> &MutationTransactionSnapshot {
        &self.state
    }

    pub fn review_context(&self) -> Result<String, String> {
        if self.state.lifecycle != MutationLifecycle::ReviewPending {
            return Err(format!(
                "parent review requires review_pending state, found {:?}",
                self.state.lifecycle
            ));
        }
        let patch = fs::read_to_string(self.root.join("prepared.patch"))
            .map_err(|error| error.to_string())?;
        Ok(format!(
            "Transactional parent review packet. This packet is the authoritative input for the review decision. The original user request has already been admitted, scoped, and executed by an isolated implementer; do not execute it again or reject the prepared commit merely because this read-only reviewer lacks mutation tools. The primary repository intentionally remains at base HEAD `{}` until authorization, so repository retrieval from the primary tree is stale with respect to prepared commit `{}` and must not be used to claim the patch is absent. Review immutable tree `{}`, exact changed-component scope {:?}, patch fingerprint `{}`, implementation evidence fingerprint `{}`, and runtime worktree verification evidence {:?}. Runtime verification evidence and the immutable patch are authoritative. Implementer narratives and earlier planning or risk summaries are advisory only; ignore any statement that conflicts with the patch or runtime evidence.\n\nPrepared patch:\n{}\n\nDecision rules: accept when the exact patch implements the scoped request, changed paths match the scope, and runtime verification evidence passes. Request revision only for a concrete defect in the prepared patch, scope, or runtime evidence. Do not request revision merely because the original request uses direct-action wording or a completion token, because the parent role is read-only, or because the primary tree still contains base content. You are not being asked to write files, run tests, or claim integration already completed. {PARENT_REVIEW_RESPONSE_REQUIREMENT}",
            self.state.base_head,
            self.state.prepared_commit,
            self.state.prepared_tree,
            self.state.changed_components,
            self.state.patch_fingerprint,
            self.state.implementation_evidence_fingerprint,
            self.state.worktree_verification_evidence,
            patch,
        ))
    }

    pub fn record_parent_review(
        &mut self,
        session: &AgentSession,
    ) -> Result<ParentReviewDecision, String> {
        let text = latest_assistant_text(session)
            .ok_or_else(|| "parent reviewer produced no assistant text".to_owned())?;
        let outcome = decode_parent_review_response(&text)?;
        self.record_review_decision(outcome.decision, outcome.rationale, session.id.as_str())
    }

    pub fn record_review_decision(
        &mut self,
        decision: ParentReviewDecision,
        rationale: impl Into<String>,
        reviewer_session_id: &str,
    ) -> Result<ParentReviewDecision, String> {
        if self.state.lifecycle != MutationLifecycle::ReviewPending {
            return Err(format!(
                "review decision requires review_pending state, found {:?}",
                self.state.lifecycle
            ));
        }
        let rationale = rationale.into();
        if reviewer_session_id.trim().is_empty() || rationale.trim().is_empty() {
            return Err("parent review receipt requires reviewer identity and rationale".to_owned());
        }
        let mut receipt = ParentReviewReceipt {
            decision: decision.clone(),
            reviewer_session_id: reviewer_session_id.to_owned(),
            commit: self.state.prepared_commit.clone(),
            tree: self.state.prepared_tree.clone(),
            patch_fingerprint: self.state.patch_fingerprint.clone(),
            scope_fingerprint: self.state.scope_fingerprint.clone(),
            evidence_fingerprint: self.state.implementation_evidence_fingerprint.clone(),
            rationale,
            fingerprint: String::new(),
        };
        receipt.fingerprint = receipt_fingerprint(&receipt);
        self.state.review = Some(receipt);
        self.state.verification = None;
        self.state.authorization = None;
        self.state.integration = None;
        match decision {
            ParentReviewDecision::Accepted => {
                self.transition(MutationLifecycle::ReviewAccepted, None)?;
            }
            ParentReviewDecision::RevisionRequested => {
                self.state.revision_count = self.state.revision_count.saturating_add(1);
                if self.state.revision_count > MAX_REVISIONS {
                    self.transition(
                        MutationLifecycle::Failed,
                        Some("parent review exceeded the bounded revision limit".to_owned()),
                    )?;
                    return Err("parent review exceeded the bounded revision limit".to_owned());
                }
                self.transition(MutationLifecycle::RevisionRequested, None)?;
            }
        }
        Ok(decision)
    }

    pub fn begin_verification(&mut self) -> Result<(), String> {
        if self.state.lifecycle != MutationLifecycle::ReviewAccepted {
            return Err(format!(
                "independent verification requires accepted review, found {:?}",
                self.state.lifecycle
            ));
        }
        self.transition(MutationLifecycle::VerificationPending, None)
    }

    pub fn verify_independently(&mut self, repo: &Path) -> Result<Vec<String>, String> {
        if self.state.lifecycle == MutationLifecycle::ReviewAccepted {
            self.begin_verification()?;
        }
        if self.state.lifecycle != MutationLifecycle::VerificationPending {
            return Err(format!(
                "independent verification requires verification_pending state, found {:?}",
                self.state.lifecycle
            ));
        }
        let manager = self.manager(repo)?;
        let components = manager
            .commit_changed_components(&self.state.prepared_commit)
            .map_err(|error| error.to_string())?;
        if changed_scope_fingerprint(&components)
            != changed_scope_fingerprint(&self.state.changed_components)
        {
            let reason = "prepared commit scope changed before verification".to_owned();
            self.fail(reason.clone())?;
            return Err(reason);
        }
        let verification_root = self.root.join("independent-verification-worktree");
        manager
            .materialize_detached_commit(&self.state.prepared_commit, &verification_root)
            .map_err(|error| error.to_string())?;
        let verification = authoritative_verification_for_components_at(
            &verification_root,
            &self.root.join("evidence/independent"),
            &self.state.repository_fingerprint,
            &self.state.prepared_commit,
            &components,
        );
        let cleanup = manager
            .remove_detached_worktree(&verification_root)
            .map_err(|error| error.to_string());
        let verification = verification.map_err(|error| error.to_string());
        if let Err(error) = cleanup {
            self.fail(format!("independent verification cleanup failed: {error}"))?;
            return Err(error);
        }
        let verification = verification?;
        if !verification.receipt.passed {
            let reason = format!(
                "independent verification failed: {}",
                verification.summary.join(" | ")
            );
            self.fail(reason.clone())?;
            return Err(reason);
        }
        let summary = verification.summary.clone();
        self.record_authoritative_verification(verification.receipt, summary.clone())?;
        Ok(summary)
    }

    #[cfg(test)]
    pub fn record_verification(
        &mut self,
        passed: bool,
        evidence: Vec<String>,
    ) -> Result<(), String> {
        if !passed {
            let reason = format!(
                "independent verification rejected the commit: {}",
                evidence.join(" | ")
            );
            self.fail(reason.clone())?;
            return Err(reason);
        }
        Err("untyped verification evidence cannot authorize integration".to_owned())
    }

    pub fn record_authoritative_verification(
        &mut self,
        authority: VerificationReceipt,
        evidence: Vec<String>,
    ) -> Result<(), String> {
        if self.state.lifecycle != MutationLifecycle::VerificationPending {
            return Err(format!(
                "verification receipt requires verification_pending state, found {:?}",
                self.state.lifecycle
            ));
        }
        authority.validate().map_err(|error| error.to_string())?;
        if !authority.passed
            || authority.plan.repository_fingerprint != self.state.repository_fingerprint
            || authority.plan.commit != self.state.prepared_commit
            || changed_scope_fingerprint(&authority.plan.components)
                != changed_scope_fingerprint(&self.state.changed_components)
            || evidence.is_empty()
        {
            let reason = "typed verification receipt does not match the prepared mutation".to_owned();
            self.fail(reason.clone())?;
            return Err(reason);
        }
        let mut receipt = IndependentVerificationReceipt {
            verifier: "runtime-independent-verifier".to_owned(),
            commit: self.state.prepared_commit.clone(),
            tree: self.state.prepared_tree.clone(),
            changed_paths: self.state.changed_paths.clone(),
            changed_components: self.state.changed_components.clone(),
            evidence,
            authority,
            fingerprint: String::new(),
        };
        receipt.fingerprint = verification_fingerprint(&receipt);
        self.state.verification = Some(receipt);
        self.transition(MutationLifecycle::Verified, None)
    }

    pub fn authorize(&mut self, repo: &Path) -> Result<(), String> {
        if self.state.lifecycle != MutationLifecycle::Verified {
            return Err(format!(
                "integration authorization requires verified state, found {:?}",
                self.state.lifecycle
            ));
        }
        let review = self
            .state
            .review
            .as_ref()
            .filter(|receipt| receipt.decision == ParentReviewDecision::Accepted)
            .ok_or_else(|| "verified mutation has no accepted parent review receipt".to_owned())?;
        let verification = self
            .state
            .verification
            .as_ref()
            .ok_or_else(|| "verified mutation has no independent verification receipt".to_owned())?;
        verification.authority.validate().map_err(|error| error.to_string())?;
        if review.commit != self.state.prepared_commit
            || verification.commit != self.state.prepared_commit
            || review.tree != self.state.prepared_tree
            || verification.tree != self.state.prepared_tree
            || !verification.authority.passed
            || verification.authority.plan.commit != self.state.prepared_commit
            || verification.authority.plan.repository_fingerprint
                != self.state.repository_fingerprint
            || changed_scope_fingerprint(&verification.authority.plan.components)
                != changed_scope_fingerprint(&self.state.changed_components)
        {
            self.fail("review or verification receipt was substituted".to_owned())?;
            return Err("review or verification receipt was substituted".to_owned());
        }
        let manager = self.manager(repo)?;
        let primary_head = manager.repository_head().map_err(|error| error.to_string())?;
        if primary_head != self.state.base_head {
            self.state.review = None;
            self.state.verification = None;
            self.state.authorization = None;
            self.state.integration = None;
            self.transition(
                MutationLifecycle::ReviewPending,
                Some(format!(
                    "primary repository drifted from {} to {}; review and verification were invalidated",
                    self.state.base_head, primary_head
                )),
            )?;
            return Err("primary repository drift requires review and verification again".to_owned());
        }
        let mut authorization = IntegrationAuthorization {
            commit: self.state.prepared_commit.clone(),
            base_head: self.state.base_head.clone(),
            primary_head,
            review_fingerprint: review.fingerprint.clone(),
            verification_fingerprint: verification.fingerprint.clone(),
            fingerprint: String::new(),
        };
        authorization.fingerprint = authorization_fingerprint(&authorization);
        self.state.authorization = Some(authorization);
        self.transition(MutationLifecycle::IntegrationAuthorized, None)
    }

    pub fn integrate(&mut self, repo: &Path) -> Result<IntegrationReceipt, String> {
        if matches!(
            self.state.lifecycle,
            MutationLifecycle::Integrated | MutationLifecycle::Reconciled
        ) {
            return self
                .state
                .integration
                .clone()
                .ok_or_else(|| "integrated transaction has no receipt".to_owned());
        }
        if self.state.lifecycle != MutationLifecycle::IntegrationAuthorized {
            return Err(format!(
                "integration requires integration_authorized state, found {:?}",
                self.state.lifecycle
            ));
        }
        let authorization = self
            .state
            .authorization
            .as_ref()
            .ok_or_else(|| "authorized transaction has no authorization receipt".to_owned())?;
        if authorization.commit != self.state.prepared_commit
            || authorization.base_head != self.state.base_head
            || authorization.fingerprint != authorization_fingerprint(authorization)
        {
            self.fail("integration authorization does not match the prepared commit".to_owned())?;
            return Err("integration authorization does not match the prepared commit".to_owned());
        }
        let manager = self.manager(repo)?;
        let receipt = manager
            .integrate_authorized(
                &self.state.worker,
                &authorization.base_head,
                &authorization.commit,
            )
            .map_err(|error| error.to_string())?;
        self.state.integration = Some(receipt.clone());
        self.transition(MutationLifecycle::Integrated, None)?;
        Ok(receipt)
    }

    pub fn reconcile(&mut self, repo: &Path) -> Result<IntegrationReceipt, String> {
        let manager = self.manager(repo)?;
        if self.state.lifecycle == MutationLifecycle::IntegrationAuthorized
            && (manager
                .commit_is_integrated(&self.state.prepared_commit)
                .map_err(|error| error.to_string())?
                || manager
                    .commit_tree_matches_head(&self.state.prepared_commit)
                    .map_err(|error| error.to_string())?)
        {
            self.state.integration = Some(self.synthetic_receipt(&manager)?);
            self.transition(MutationLifecycle::Integrated, None)?;
        }
        if self.state.lifecycle == MutationLifecycle::Reconciled {
            manager
                .cleanup(std::slice::from_ref(&self.state.worker))
                .map_err(|error| error.to_string())?;
            return self
                .state
                .integration
                .clone()
                .ok_or_else(|| "reconciled transaction has no integration receipt".to_owned());
        }
        if self.state.lifecycle != MutationLifecycle::Integrated {
            return Err(format!(
                "reconciliation requires integrated state, found {:?}",
                self.state.lifecycle
            ));
        }
        if !manager
            .commit_is_integrated(&self.state.prepared_commit)
            .map_err(|error| error.to_string())?
            && !manager
                .commit_tree_matches_head(&self.state.prepared_commit)
                .map_err(|error| error.to_string())?
        {
            return Err("integrated transaction cannot be reconciled against primary HEAD".to_owned());
        }
        let receipt = self
            .state
            .integration
            .clone()
            .unwrap_or(self.synthetic_receipt(&manager)?);
        self.state.integration = Some(receipt.clone());
        self.transition(MutationLifecycle::Reconciled, None)?;
        manager
            .cleanup(std::slice::from_ref(&self.state.worker))
            .map_err(|error| error.to_string())?;
        Ok(receipt)
    }

    pub fn cancel(&mut self, reason: impl Into<String>) -> Result<(), String> {
        if matches!(
            self.state.lifecycle,
            MutationLifecycle::Reconciled | MutationLifecycle::Cancelled | MutationLifecycle::Failed
        ) {
            return Ok(());
        }
        let reason = reason.into();
        if reason.trim().is_empty() {
            return Err("transaction cancellation requires a reason".to_owned());
        }
        self.transition(MutationLifecycle::Cancelled, Some(reason))
    }

    pub fn fail(&mut self, reason: impl Into<String>) -> Result<(), String> {
        if matches!(
            self.state.lifecycle,
            MutationLifecycle::Reconciled | MutationLifecycle::Cancelled | MutationLifecycle::Failed
        ) {
            return Ok(());
        }
        let reason = reason.into();
        if reason.trim().is_empty() {
            return Err("transaction failure requires a reason".to_owned());
        }
        self.transition(MutationLifecycle::Failed, Some(reason))
    }

    pub fn emit(&self, events: &Sender<RuntimeEvent>) {
        let (kind, title) = match self.state.lifecycle {
            MutationLifecycle::Planned => (RuntimeActivityKind::Progress, "Mutation planned"),
            MutationLifecycle::PreparedInIsolation => {
                (RuntimeActivityKind::Progress, "Prepared in isolation")
            }
            MutationLifecycle::ReviewPending => (RuntimeActivityKind::Progress, "Parent review pending"),
            MutationLifecycle::RevisionRequested => {
                (RuntimeActivityKind::Progress, "Revision requested")
            }
            MutationLifecycle::ReviewAccepted => (RuntimeActivityKind::Progress, "Review accepted"),
            MutationLifecycle::VerificationPending => {
                (RuntimeActivityKind::Progress, "Independent verification pending")
            }
            MutationLifecycle::Verified => (RuntimeActivityKind::Progress, "Prepared commit verified"),
            MutationLifecycle::IntegrationAuthorized => {
                (RuntimeActivityKind::Progress, "Integration authorized")
            }
            MutationLifecycle::Integrated => (RuntimeActivityKind::Progress, "Prepared commit integrated"),
            MutationLifecycle::Reconciled => (RuntimeActivityKind::Done, "Mutation reconciled"),
            MutationLifecycle::Cancelled => (RuntimeActivityKind::Done, "Mutation cancelled"),
            MutationLifecycle::Failed => (RuntimeActivityKind::Done, "Mutation failed"),
        };
        let _ = events.send(RuntimeEvent::Activity(RuntimeActivity {
            id: Some(self.state.transaction_id.clone()),
            kind,
            title: title.to_owned(),
            details: vec![
                format!("state={:?}", self.state.lifecycle),
                format!("commit={}", self.state.prepared_commit),
                format!("revision={}", self.state.revision),
                format!("changed_paths={:?}", self.state.changed_paths),
            ],
        }));
    }

    fn prepare(
        &mut self,
        manager: &WorkerManager,
        input: PreparedMutationInput,
    ) -> Result<(), String> {
        if !matches!(
            self.state.lifecycle,
            MutationLifecycle::Planned | MutationLifecycle::RevisionRequested
        ) {
            if self.state.prepared_commit == input.worker.commit.clone().unwrap_or_default() {
                return Ok(());
            }
            return Err(format!(
                "prepared commit cannot replace transaction in {:?} state",
                self.state.lifecycle
            ));
        }
        let commit = input
            .worker
            .commit
            .clone()
            .ok_or_else(|| "prepared worker has no commit".to_owned())?;
        let tree = manager.commit_tree(&commit).map_err(|error| error.to_string())?;
        let changed_components = manager
            .commit_changed_components(&commit)
            .map_err(|error| error.to_string())?;
        let changed_paths = manager
            .commit_changed_paths(&commit)
            .map_err(|error| error.to_string())?;
        input
            .worktree_verification_receipt
            .validate()
            .map_err(|error| error.to_string())?;
        if changed_paths != input.changed_paths
            || changed_scope_fingerprint(&changed_components)
                != changed_scope_fingerprint(&input.changed_components)
            || !input.worktree_verification_receipt.passed
        {
            return Err(format!(
                "prepared commit paths do not match verified implementation scope: {:?} != {:?}",
                changed_paths, input.changed_paths
            ));
        }
        let patch = manager
            .commit_patch(&input.base_head, &commit)
            .map_err(|error| error.to_string())?;
        if patch.trim().is_empty() {
            return Err("prepared commit patch is empty".to_owned());
        }
        fs::write(self.root.join("prepared.patch"), patch.as_bytes())
            .map_err(|error| error.to_string())?;
        self.state.worker = input.worker;
        self.state.prepared_commit = commit;
        self.state.prepared_tree = tree;
        self.state.changed_paths = changed_paths;
        self.state.changed_components = changed_components;
        self.state.patch_fingerprint = hash(&patch);
        self.state.scope_fingerprint = changed_scope_fingerprint(&self.state.changed_components);
        self.state.implementation_summary = input.implementation_summary;
        self.state.worktree_verification_evidence = input.worktree_verification_evidence;
        self.state.worktree_verification_receipt = Some(input.worktree_verification_receipt);
        self.state.implementation_evidence_fingerprint = hash(&(
            &self.state.implementation_summary,
            &self.state.worktree_verification_evidence,
        ));
        self.state.review = None;
        self.state.verification = None;
        self.state.authorization = None;
        self.state.integration = None;
        self.transition(MutationLifecycle::PreparedInIsolation, None)?;
        self.transition(MutationLifecycle::ReviewPending, None)
    }

    fn validate_identity(&self, input: &PreparedMutationInput) -> Result<(), String> {
        if self.state.plan_fingerprint != input.plan_fingerprint
            || self.state.repository_fingerprint != input.repository_fingerprint
            || self.state.task_id != input.task_id
            || self.state.base_head != input.base_head
            || self.state.worker.id != input.worker.id
        {
            return Err("prepared mutation does not match the durable transaction identity".to_owned());
        }
        Ok(())
    }

    fn manager(&self, repo: &Path) -> Result<WorkerManager, String> {
        WorkerManager::new(repo, self.root.join("worktrees")).map_err(|error| error.to_string())
    }

    fn synthetic_receipt(&self, manager: &WorkerManager) -> Result<IntegrationReceipt, String> {
        Ok(IntegrationReceipt {
            worker_id: self.state.worker.id.clone(),
            branch: self.state.worker.branch.clone(),
            commit: self.state.prepared_commit.clone(),
            base_head: self.state.base_head.clone(),
            integrated_head: manager.repository_head().map_err(|error| error.to_string())?,
            changed_paths: self.state.changed_paths.clone(),
            changed_components: self.state.changed_components.clone(),
        })
    }

    fn transition(
        &mut self,
        lifecycle: MutationLifecycle,
        last_error: Option<String>,
    ) -> Result<(), String> {
        self.state.lifecycle = lifecycle;
        self.state.last_error = last_error;
        self.state.revision = self.state.revision.saturating_add(1);
        self.refresh();
        self.persist()
    }

    fn refresh(&mut self) {
        self.state.fingerprint = transaction_fingerprint(&self.state);
    }

    fn persist(&self) -> Result<(), String> {
        write_atomic(&self.path, &self.state)
    }

    fn validate(&self) -> Result<(), String> {
        if self.state.schema_version != TRANSACTION_SCHEMA_VERSION
            || self.state.transaction_id.trim().is_empty()
            || self.state.plan_fingerprint.trim().is_empty()
            || self.state.repository_fingerprint.trim().is_empty()
            || self.state.task_id.trim().is_empty()
            || self.state.base_head.trim().is_empty()
            || self.state.worker.id.trim().is_empty()
            || self.state.fingerprint != transaction_fingerprint(&self.state)
        {
            return Err("durable mutation transaction is incomplete or corrupted".to_owned());
        }
        if !matches!(self.state.lifecycle, MutationLifecycle::Planned)
            && (self.state.prepared_commit.trim().is_empty()
                || self.state.prepared_tree.trim().is_empty()
                || self.state.changed_paths.is_empty()
                || self.state.changed_components.is_empty()
                || self.state.patch_fingerprint.trim().is_empty()
                || self.state.scope_fingerprint.trim().is_empty()
                || self.state.implementation_evidence_fingerprint.trim().is_empty())
        {
            return Err("prepared mutation transaction is missing immutable review inputs".to_owned());
        }
        if let Some(review) = self.state.review.as_ref()
            && review.fingerprint != receipt_fingerprint(review)
        {
            return Err("parent review receipt fingerprint is invalid".to_owned());
        }
        if let Some(worktree) = self.state.worktree_verification_receipt.as_ref() {
            worktree.validate().map_err(|error| error.to_string())?;
        }
        if let Some(verification) = self.state.verification.as_ref()
            && (verification.fingerprint != verification_fingerprint(verification)
                || verification.authority.validate().is_err())
        {
            return Err("independent verification receipt fingerprint is invalid".to_owned());
        }
        if let Some(authorization) = self.state.authorization.as_ref()
            && authorization.fingerprint != authorization_fingerprint(authorization)
        {
            return Err("integration authorization fingerprint is invalid".to_owned());
        }
        Ok(())
    }
}

pub fn complete_after_parent_review(
    path: &Path,
    repo: &Path,
    session: &AgentSession,
    events: &Sender<RuntimeEvent>,
) -> Result<TransactionCompletion, String> {
    let mut transaction = MutationTransaction::open(path)?;
    match transaction.record_parent_review(session)? {
        ParentReviewDecision::RevisionRequested => {
            let rationale = transaction
                .state
                .review
                .as_ref()
                .map(|receipt| receipt.rationale.clone())
                .unwrap_or_else(|| "parent requested revision".to_owned());
            transaction.emit(events);
            Ok(TransactionCompletion::RevisionRequested(rationale))
        }
        ParentReviewDecision::Accepted => {
            transaction.emit(events);
            transaction.begin_verification()?;
            transaction.emit(events);
            transaction.verify_independently(repo)?;
            transaction.emit(events);
            transaction.authorize(repo)?;
            transaction.emit(events);
            transaction.integrate(repo)?;
            transaction.emit(events);
            let receipt = transaction.reconcile(repo)?;
            transaction.emit(events);
            Ok(TransactionCompletion::Reconciled(receipt))
        }
    }
}

pub fn cancel_transaction(
    path: &Path,
    reason: &str,
    events: &Sender<RuntimeEvent>,
) -> Result<(), String> {
    let mut transaction = MutationTransaction::open(path)?;
    transaction.cancel(reason)?;
    transaction.emit(events);
    Ok(())
}

pub fn fail_transaction(
    path: &Path,
    reason: &str,
    events: &Sender<RuntimeEvent>,
) -> Result<(), String> {
    let mut transaction = MutationTransaction::open(path)?;
    transaction.fail(reason)?;
    transaction.emit(events);
    Ok(())
}

fn latest_assistant_text(session: &AgentSession) -> Option<String> {
    session.messages.iter().rev().find_map(|message| {
        if message.role != Role::Assistant {
            return None;
        }
        let text = message
            .content
            .iter()
            .filter_map(|block| match block {
                MessageBlock::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        (!text.trim().is_empty()).then_some(text)
    })
}

fn decode_parent_review_response(text: &str) -> Result<ParentReviewOutcome, String> {
    let final_line = final_parent_review_line(text).map_err(|error| error.to_string())?;
    let response: ParentReviewResponse = serde_json::from_str(final_line).map_err(|error| {
        ParentReviewResponseError::InvalidEnvelope(error.to_string()).to_string()
    })?;
    validate_parent_review_response(response, final_line).map_err(|error| error.to_string())
}

fn transaction_fingerprint(state: &MutationTransactionSnapshot) -> String {
    let mut canonical = state.clone();
    canonical.fingerprint.clear();
    hash(&canonical)
}

fn receipt_fingerprint(receipt: &ParentReviewReceipt) -> String {
    let mut canonical = receipt.clone();
    canonical.fingerprint.clear();
    hash(&canonical)
}

fn verification_fingerprint(receipt: &IndependentVerificationReceipt) -> String {
    let mut canonical = receipt.clone();
    canonical.fingerprint.clear();
    hash(&canonical)
}

fn authorization_fingerprint(authorization: &IntegrationAuthorization) -> String {
    let mut canonical = authorization.clone();
    canonical.fingerprint.clear();
    hash(&canonical)
}

fn hash<T: Serialize>(value: &T) -> String {
    let bytes = serde_json::to_vec(value).unwrap_or_default();
    format!("{:x}", Sha256::digest(bytes))
}

fn write_atomic<T: Serialize>(path: &Path, value: &T) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| "durable transaction path has no parent".to_owned())?;
    fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    let temporary = path.with_extension("json.tmp");
    fs::write(
        &temporary,
        serde_json::to_vec_pretty(value).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    #[cfg(windows)]
    if path.exists() {
        fs::remove_file(path).map_err(|error| error.to_string())?;
    }
    fs::rename(temporary, path).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn git(repo: &Path, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(repo)
            .output()
            .expect("git");
        assert!(
            output.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn fixture() -> (tempfile::TempDir, PathBuf, WorkerManager, PreparedMutationInput) {
        let directory = tempfile::tempdir().expect("tempdir");
        let repo = directory.path().join("repo");
        fs::create_dir(&repo).expect("repo");
        git(&repo, &["init", "-b", "main"]);
        git(&repo, &["config", "core.autocrlf", "false"]);
        git(&repo, &["config", "user.name", "Medusa Test"]);
        git(&repo, &["config", "user.email", "medusa@example.invalid"]);
        fs::create_dir_all(repo.join("src")).expect("src");
        fs::write(repo.join("src/lib.rs"), "pub fn value() -> u32 { 1 }\n").expect("source");
        #[cfg(windows)]
        fs::write(
            repo.join("verify.ps1"),
            "$ErrorActionPreference='Stop'\nif (-not (Test-Path src/lib.rs)) { exit 1 }\nWrite-Output 'verification-ok'\n",
        )
        .expect("verify");
        #[cfg(not(windows))]
        fs::write(
            repo.join("verify.sh"),
            "#!/bin/sh\nset -eu\ntest -f src/lib.rs\necho verification-ok\n",
        )
        .expect("verify");
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-m", "base"]);
        let root = repo.join(".medusa/executions/test");
        let manager = WorkerManager::new(&repo, root.join("worktrees")).expect("manager");
        let base_head = manager.repository_head().expect("head");
        let mut worker = manager
            .open_or_create_worker("transaction", "worker-implement")
            .expect("worker");
        fs::write(
            worker.worktree.join("src/lib.rs"),
            "pub fn value() -> u32 { 2 }\n",
        )
        .expect("change");
        worker = manager
            .finalize_worker(worker, &base_head, "prepared change")
            .expect("finalize");
        let commit = worker.commit.clone().expect("commit");
        let changed_components = manager
            .commit_changed_components(&commit)
            .expect("components");
        let worktree_verification = authoritative_verification_for_components_at(
            &worker.worktree,
            &root.join("evidence/test-worktree"),
            "repo",
            &format!("worktree:{}", changed_scope_fingerprint(&changed_components)),
            &changed_components,
        )
        .expect("worktree verification");
        let input = PreparedMutationInput {
            plan_fingerprint: "plan".to_owned(),
            repository_fingerprint: "repo".to_owned(),
            task_id: "implement".to_owned(),
            base_head,
            worker,
            changed_paths: vec!["src/lib.rs".to_owned()],
            changed_components,
            implementation_summary: "changed the value".to_owned(),
            worktree_verification_evidence: worktree_verification.summary,
            worktree_verification_receipt: worktree_verification.receipt,
        };
        (directory, repo, manager, input)
    }

    #[test]
    fn typed_parent_review_envelope_is_required_at_runtime_boundary() {
        let accepted = decode_parent_review_response(
            "The prepared patch is correct.\n{\"schema_version\":1,\"decision\":\"accepted\",\"rationale\":\"exact patch and evidence agree\"}",
        )
        .expect("typed response");
        assert_eq!(accepted.decision, ParentReviewDecision::Accepted);
        assert_eq!(accepted.response_fingerprint.len(), 64);

        assert!(decode_parent_review_response(
            "MEDUSA_REVIEW_ACCEPTED: exact patch and evidence agree"
        )
        .is_err());
        assert!(decode_parent_review_response(
            "{\"schema_version\":1,\"decision\":\"accepted\",\"rationale\":\"ok\",\"extra\":true}"
        )
        .is_err());
        assert!(decode_parent_review_response(
            "{\"schema_version\":1,\"decision\":\"accepted\",\"rationale\":\"ok\"}\ntrailing"
        )
        .is_err());
    }

    #[test]
    fn review_context_prioritizes_prepared_artifact_and_runtime_evidence() {
        let (_directory, repo, _manager, mut input) = fixture();
        input.implementation_summary =
            "reviewer lacks write tools and the primary tree is unchanged".to_owned();
        input.worktree_verification_evidence = vec![
            "test result: ok. 1 passed; 0 failed".to_owned(),
        ];
        let root = repo.join(".medusa/executions/test");
        let transaction =
            MutationTransaction::open_or_prepare(&root, &repo, input).expect("transaction");
        let context = transaction.review_context().expect("review context");
        assert!(context.contains("authoritative input for the review decision"));
        assert!(context.contains("primary repository intentionally remains at base HEAD"));
        assert!(context.contains("repository retrieval from the primary tree is stale"));
        assert!(context.contains("test result: ok. 1 passed; 0 failed"));
        assert!(context.contains("Implementer narratives and earlier planning or risk summaries are advisory only"));
        assert!(context.contains("Do not request revision merely because"));
        assert!(context.contains("\"schema_version\":1"));
        assert!(context.contains("Free-form markers"));
    }

    #[test]
    fn revision_request_preserves_primary_repository() {
        let (_directory, repo, _manager, input) = fixture();
        let root = repo.join(".medusa/executions/test");
        let mut transaction =
            MutationTransaction::open_or_prepare(&root, &repo, input).expect("transaction");
        transaction
            .record_review_decision(
                ParentReviewDecision::RevisionRequested,
                "add a missing regression test",
                "parent-session",
            )
            .expect("review");
        assert_eq!(transaction.state.lifecycle, MutationLifecycle::RevisionRequested);
        assert_eq!(
            fs::read_to_string(repo.join("src/lib.rs")).expect("primary"),
            "pub fn value() -> u32 { 1 }\n"
        );
        assert!(transaction.state.worker.worktree.exists());
    }

    #[test]
    fn verification_failure_preserves_primary_repository() {
        let (_directory, repo, _manager, input) = fixture();
        let root = repo.join(".medusa/executions/test");
        let mut transaction =
            MutationTransaction::open_or_prepare(&root, &repo, input).expect("transaction");
        transaction
            .record_review_decision(
                ParentReviewDecision::Accepted,
                "immutable commit is acceptable",
                "parent-session",
            )
            .expect("review");
        transaction.begin_verification().expect("begin verification");
        assert!(transaction
            .record_verification(false, vec!["regression".to_owned()])
            .is_err());
        assert_eq!(
            fs::read_to_string(repo.join("src/lib.rs")).expect("primary"),
            "pub fn value() -> u32 { 1 }\n"
        );
    }

    #[test]
    fn review_verification_authorization_and_integration_are_idempotent() {
        let (_directory, repo, _manager, input) = fixture();
        let root = repo.join(".medusa/executions/test");
        let mut transaction =
            MutationTransaction::open_or_prepare(&root, &repo, input).expect("transaction");
        transaction
            .record_review_decision(
                ParentReviewDecision::Accepted,
                "reviewed exact patch",
                "parent-session",
            )
            .expect("review");
        transaction.verify_independently(&repo).expect("verification");
        transaction.authorize(&repo).expect("authorize");
        let first = transaction.integrate(&repo).expect("integrate");
        let second = transaction.integrate(&repo).expect("duplicate integrate");
        assert_eq!(first, second);
        let reconciled = transaction.reconcile(&repo).expect("reconcile");
        assert_eq!(first, reconciled);
        assert_eq!(
            fs::read_to_string(repo.join("src/lib.rs")).expect("primary"),
            "pub fn value() -> u32 { 2 }\n"
        );
        assert!(!transaction.state.worker.worktree.exists());
        let mut reopened = MutationTransaction::open(transaction.path()).expect("reopen");
        assert_eq!(reopened.reconcile(&repo).expect("repeat reconcile"), first);
    }

    #[test]
    fn primary_drift_invalidates_review_and_verification() {
        let (_directory, repo, _manager, input) = fixture();
        let root = repo.join(".medusa/executions/test");
        let mut transaction =
            MutationTransaction::open_or_prepare(&root, &repo, input).expect("transaction");
        transaction
            .record_review_decision(
                ParentReviewDecision::Accepted,
                "reviewed exact patch",
                "parent-session",
            )
            .expect("review");
        transaction.verify_independently(&repo).expect("verification");
        fs::write(repo.join("drift.txt"), "drift\n").expect("drift");
        git(&repo, &["add", "drift.txt"]);
        git(&repo, &["commit", "-m", "drift"]);
        assert!(transaction.authorize(&repo).is_err());
        assert_eq!(transaction.state.lifecycle, MutationLifecycle::ReviewPending);
        assert!(transaction.state.review.is_none());
        assert!(transaction.state.verification.is_none());
    }

    #[test]
    fn transaction_reopens_at_each_persisted_boundary() {
        let (_directory, repo, _manager, input) = fixture();
        let root = repo.join(".medusa/executions/test");
        let mut transaction =
            MutationTransaction::open_or_prepare(&root, &repo, input).expect("transaction");
        assert_eq!(
            MutationTransaction::open(transaction.path())
                .expect("review pending")
                .state
                .lifecycle,
            MutationLifecycle::ReviewPending
        );
        transaction
            .record_review_decision(
                ParentReviewDecision::Accepted,
                "reviewed",
                "parent-session",
            )
            .expect("review");
        assert_eq!(
            MutationTransaction::open(transaction.path())
                .expect("review accepted")
                .state
                .lifecycle,
            MutationLifecycle::ReviewAccepted
        );
        transaction.verify_independently(&repo).expect("verified");
        transaction.authorize(&repo).expect("authorized");
        assert_eq!(
            MutationTransaction::open(transaction.path())
                .expect("authorized reopen")
                .state
                .lifecycle,
            MutationLifecycle::IntegrationAuthorized
        );
    }
}
