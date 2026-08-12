use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use medusa_github::{AuthStatus, CommandExecutor, GitHubService};
use medusa_improvement::ImprovementProposal;
use medusa_transaction_coordinator::{
    CoordinatorError, Decision, Participant, TransactionCoordinator, TransactionPhase, Vote,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{Capability, CapabilityRegistry, CapabilityState};

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityPermission {
    Read,
    Write,
    RepositoryMutation,
    RuntimeMutation,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CapabilityDescriptor {
    pub id: String,
    pub capability: Capability,
    pub description: String,
    pub permissions: BTreeSet<CapabilityPermission>,
    pub explicit_approval: BTreeSet<CapabilityPermission>,
}

#[must_use]
pub fn github_descriptor() -> CapabilityDescriptor {
    CapabilityDescriptor {
        id: "github_operations".into(),
        capability: Capability::GitHub,
        description:
            "Authenticated GitHub reads and explicitly approved writes through medusa-github".into(),
        permissions: BTreeSet::from([
            CapabilityPermission::Read,
            CapabilityPermission::Write,
            CapabilityPermission::RepositoryMutation,
        ]),
        explicit_approval: BTreeSet::from([
            CapabilityPermission::Write,
            CapabilityPermission::RepositoryMutation,
        ]),
    }
}

#[must_use]
pub fn self_improvement_descriptor() -> CapabilityDescriptor {
    CapabilityDescriptor {
        id: "self_improvement".into(), capability: Capability::SelfImprovement,
        description: "Reviewable, verified and rollback-capable improvements committed through the transaction coordinator".into(),
        permissions: BTreeSet::from([CapabilityPermission::Read, CapabilityPermission::RepositoryMutation, CapabilityPermission::RuntimeMutation]),
        explicit_approval: BTreeSet::from([CapabilityPermission::RepositoryMutation, CapabilityPermission::RuntimeMutation]),
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CapabilityGrant {
    pub capability: Capability,
    pub permissions: BTreeSet<CapabilityPermission>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CapabilityAuditEvent {
    pub sequence: u64,
    pub descriptor_id: String,
    pub capability: Capability,
    pub permission: CapabilityPermission,
    pub allowed: bool,
    pub explicit_approval: bool,
    pub reason: String,
}

pub struct CapabilityAuthorizer {
    registry: CapabilityRegistry,
    grants: BTreeMap<Capability, BTreeSet<CapabilityPermission>>,
    events: Vec<CapabilityAuditEvent>,
}

impl CapabilityAuthorizer {
    #[must_use]
    pub fn new(registry: CapabilityRegistry, grants: Vec<CapabilityGrant>) -> Self {
        Self {
            registry,
            grants: grants
                .into_iter()
                .map(|g| (g.capability, g.permissions))
                .collect(),
            events: Vec::new(),
        }
    }

    pub fn require(
        &mut self,
        descriptor: &CapabilityDescriptor,
        permission: CapabilityPermission,
        explicit_approval: bool,
    ) -> MedusaResult<()> {
        let available = self.registry.available(descriptor.capability);
        let declared = descriptor.permissions.contains(&permission);
        let granted = self
            .grants
            .get(&descriptor.capability)
            .is_some_and(|p| p.contains(&permission));
        let approval_satisfied =
            !descriptor.explicit_approval.contains(&permission) || explicit_approval;
        let allowed = available && declared && granted && approval_satisfied;
        let reason = if !available {
            self.registry.state(descriptor.capability).detail
        } else if !declared {
            "permission is not declared by the capability".into()
        } else if !granted {
            "permission was not granted".into()
        } else if !approval_satisfied {
            "explicit approval is required".into()
        } else {
            "capability permission granted".into()
        };
        self.events.push(CapabilityAuditEvent {
            sequence: self.events.len() as u64 + 1,
            descriptor_id: descriptor.id.clone(),
            capability: descriptor.capability,
            permission,
            allowed,
            explicit_approval,
            reason: reason.clone(),
        });
        if allowed {
            Ok(())
        } else {
            Err(policy_denied(reason))
        }
    }
    #[must_use]
    pub fn events(&self) -> &[CapabilityAuditEvent] {
        &self.events
    }
    #[must_use]
    pub fn registry(&self) -> &CapabilityRegistry {
        &self.registry
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PendingGitHubIssue {
    pub id: String,
    pub title: String,
    pub body: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ImprovementPatchProposal {
    pub proposal: ImprovementProposal,
    pub rationale: String,
    pub unified_diff: String,
    pub verification_commands: Vec<String>,
    pub rollback_metadata: String,
}

impl ImprovementPatchProposal {
    pub fn validate(&self) -> MedusaResult<()> {
        self.proposal.validate()?;
        if self.rationale.trim().is_empty()
            || self.unified_diff.trim().is_empty()
            || self.verification_commands.is_empty()
            || self
                .verification_commands
                .iter()
                .any(|c| c.trim().is_empty())
            || self.rollback_metadata.trim().is_empty()
        {
            return Err(invalid("improvement patch proposal is incomplete"));
        }
        let diff_paths = paths_from_unified_diff(&self.unified_diff)?;
        let declared = self
            .proposal
            .touched_paths
            .iter()
            .map(|p| normalize_path(p))
            .collect::<BTreeSet<_>>();
        if diff_paths != declared {
            return Err(invalid(
                "proposal touched_paths must exactly match paths derived from unified_diff",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct VerificationEvidence {
    pub command_results: Vec<String>,
}
impl VerificationEvidence {
    pub fn validate(&self, expected_commands: &[String]) -> MedusaResult<()> {
        if self.command_results.len() != expected_commands.len()
            || self.command_results.iter().any(|r| r.trim().is_empty())
        {
            return Err(invalid(
                "successful verification evidence is required for every command",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CapabilityDiagnostics {
    pub states: BTreeMap<Capability, CapabilityState>,
    pub descriptors: Vec<CapabilityDescriptor>,
    pub audit_events: Vec<CapabilityAuditEvent>,
}

pub struct ExplicitCapabilityRuntime<E> {
    authorizer: CapabilityAuthorizer,
    github: GitHubService<E>,
    coordinator: TransactionCoordinator,
    pending_github_issues: BTreeMap<String, PendingGitHubIssue>,
    pending_improvements: BTreeMap<String, ImprovementPatchProposal>,
}

impl<E: CommandExecutor> ExplicitCapabilityRuntime<E> {
    #[must_use]
    pub fn new(
        registry: CapabilityRegistry,
        grants: Vec<CapabilityGrant>,
        github: GitHubService<E>,
    ) -> Self {
        Self {
            authorizer: CapabilityAuthorizer::new(registry, grants),
            github,
            coordinator: TransactionCoordinator::default(),
            pending_github_issues: BTreeMap::new(),
            pending_improvements: BTreeMap::new(),
        }
    }
    pub fn github_auth_status(&mut self) -> MedusaResult<AuthStatus> {
        self.authorizer
            .require(&github_descriptor(), CapabilityPermission::Read, false)?;
        self.github.auth_status()
    }
    pub fn propose_github_issue(
        &mut self,
        id: impl Into<String>,
        title: impl Into<String>,
        body: impl Into<String>,
    ) -> MedusaResult<PendingGitHubIssue> {
        self.authorizer
            .require(&github_descriptor(), CapabilityPermission::Read, false)?;
        let proposal = PendingGitHubIssue {
            id: id.into(),
            title: title.into(),
            body: body.into(),
        };
        if proposal.id.trim().is_empty()
            || proposal.title.trim().is_empty()
            || proposal.body.trim().is_empty()
        {
            return Err(invalid("GitHub issue proposal is incomplete"));
        }
        if self.pending_github_issues.contains_key(&proposal.id) {
            return Err(invalid("GitHub issue proposal id must be unique"));
        }
        self.pending_github_issues
            .insert(proposal.id.clone(), proposal.clone());
        Ok(proposal)
    }
    pub fn approve_github_issue(
        &mut self,
        proposal_id: &str,
        explicit_approval: bool,
    ) -> MedusaResult<String> {
        self.authorizer.require(
            &github_descriptor(),
            CapabilityPermission::Write,
            explicit_approval,
        )?;
        let proposal = self
            .pending_github_issues
            .get(proposal_id)
            .cloned()
            .ok_or_else(|| invalid("GitHub issue proposal does not exist"))?;
        let output = self.github.create_issue(&proposal.title, &proposal.body)?;
        self.pending_github_issues.remove(proposal_id);
        Ok(output)
    }
    pub fn stage_improvement(
        &mut self,
        patch: ImprovementPatchProposal,
        sensitive_change_approval: bool,
    ) -> MedusaResult<String> {
        self.authorizer.require(
            &self_improvement_descriptor(),
            CapabilityPermission::Read,
            false,
        )?;
        patch.validate()?;
        let transaction_id = format!("improvement-{}", patch.proposal.id);
        if self.pending_improvements.contains_key(&transaction_id) {
            return Err(invalid(
                "improvement proposal id must be unique while pending",
            ));
        }
        if patch
            .proposal
            .touched_paths
            .iter()
            .any(|p| protected_path(p))
            && !sensitive_change_approval
        {
            return Err(policy_denied(
                "policy, sandbox, approval, credential, capability, or update-trust paths require explicit approval",
            ));
        }
        let participant = Participant {
            worker_id: "self-improvement".into(),
            lease_epoch: 1,
            intent_fingerprint: fingerprint(patch.unified_diff.as_bytes()),
        };
        self.coordinator
            .create(
                transaction_id.clone(),
                patch.proposal.id.clone(),
                1,
                vec![participant],
                fingerprint(patch.rationale.as_bytes()),
            )
            .map_err(coordinator_error)?;
        self.coordinator
            .transition(&transaction_id, TransactionPhase::Resolving)
            .map_err(coordinator_error)?;
        self.coordinator
            .transition(&transaction_id, TransactionPhase::Preparing)
            .map_err(coordinator_error)?;
        self.coordinator
            .transition(&transaction_id, TransactionPhase::Prepared)
            .map_err(coordinator_error)?;
        self.pending_improvements
            .insert(transaction_id.clone(), patch);
        Ok(transaction_id)
    }
    pub fn approve_improvement(
        &mut self,
        transaction_id: &str,
        explicit_approval: bool,
        mut verify: impl FnMut(&ImprovementPatchProposal) -> MedusaResult<VerificationEvidence>,
        mut commit: impl FnMut(&ImprovementPatchProposal) -> MedusaResult<()>,
        mut rollback: impl FnMut(&ImprovementPatchProposal) -> MedusaResult<()>,
    ) -> MedusaResult<()> {
        self.authorizer.require(
            &self_improvement_descriptor(),
            CapabilityPermission::RepositoryMutation,
            explicit_approval,
        )?;
        let patch = self
            .pending_improvements
            .get(transaction_id)
            .cloned()
            .ok_or_else(|| invalid("improvement transaction does not exist"))?;
        let evidence = verify(&patch)?;
        evidence.validate(&patch.verification_commands)?;
        let verification = fingerprint(
            serde_json::to_string(&evidence)
                .map_err(|e| invalid(e.to_string()))?
                .as_bytes(),
        );
        let rollback_fingerprint = fingerprint(patch.rollback_metadata.as_bytes());
        self.coordinator
            .attach_evidence(
                transaction_id,
                Some(verification),
                Some(rollback_fingerprint),
                None,
            )
            .map_err(coordinator_error)?;
        self.coordinator
            .vote(
                transaction_id,
                Vote::Prepared {
                    worker_id: "self-improvement".into(),
                    lease_epoch: 1,
                },
            )
            .map_err(coordinator_error)?;
        if self
            .coordinator
            .decide(transaction_id)
            .map_err(coordinator_error)?
            != Decision::Commit
        {
            return Err(policy_denied("improvement transaction was not committed"));
        }
        self.coordinator
            .transition(transaction_id, TransactionPhase::Committing)
            .map_err(coordinator_error)?;
        if let Err(commit_error) = commit(&patch) {
            self.coordinator
                .transition(transaction_id, TransactionPhase::Recovering)
                .map_err(coordinator_error)?;
            let rollback_result = rollback(&patch);
            self.coordinator
                .transition(transaction_id, TransactionPhase::Failed)
                .map_err(coordinator_error)?;
            return match rollback_result {
                Ok(()) => Err(commit_error),
                Err(rollback_error) => Err(MedusaError::new(
                    ErrorCode::InternalInvariant,
                    ErrorCategory::Internal,
                    format!("commit failed: {commit_error}; rollback failed: {rollback_error}"),
                )),
            };
        }
        self.coordinator
            .transition(transaction_id, TransactionPhase::Committed)
            .map_err(coordinator_error)?;
        self.pending_improvements.remove(transaction_id);
        Ok(())
    }
    #[must_use]
    pub fn transaction_phase(&self, id: &str) -> Option<TransactionPhase> {
        self.coordinator.record(id).map(|r| r.phase.clone())
    }
    #[must_use]
    pub fn diagnostics(&self) -> CapabilityDiagnostics {
        CapabilityDiagnostics {
            states: self.authorizer.registry().capabilities.clone(),
            descriptors: vec![github_descriptor(), self_improvement_descriptor()],
            audit_events: self.authorizer.events().to_vec(),
        }
    }
}

fn paths_from_unified_diff(diff: &str) -> MedusaResult<BTreeSet<String>> {
    let mut paths = BTreeSet::new();
    for line in diff.lines() {
        let raw = line
            .strip_prefix("+++ ")
            .or_else(|| line.strip_prefix("--- "))
            .or_else(|| line.strip_prefix("rename to "))
            .or_else(|| line.strip_prefix("rename from "));
        if let Some(raw) = raw {
            let raw = raw.split('\t').next().unwrap_or(raw).trim();
            if raw == "/dev/null" {
                continue;
            }
            let raw = raw
                .strip_prefix("a/")
                .or_else(|| raw.strip_prefix("b/"))
                .unwrap_or(raw);
            if raw.is_empty() || raw.starts_with('/') || raw.split('/').any(|part| part == "..") {
                return Err(invalid("unified_diff contains an unsafe path"));
            }
            paths.insert(normalize_path(Path::new(raw)));
        }
    }
    if paths.is_empty() {
        return Err(invalid("unified_diff does not declare any changed paths"));
    }
    Ok(paths)
}

fn normalize_path(path: &Path) -> String {
    path.to_string_lossy()
        .replace('\\', "/")
        .trim_start_matches("./")
        .to_owned()
}

fn protected_path(path: &Path) -> bool {
    let p = normalize_path(path);
    [
        ".github/workflows",
        ".medusa/policy",
        "crates/medusa-hardening",
        "crates/medusa-capabilities",
        "crates/medusa-config",
        "crates/medusa-update",
        "crates/medusa-agent/src/approval.rs",
        "crates/medusa-agent/src/policy.rs",
        "crates/medusa-agent/src/windows_sandbox.rs",
        "apps/medusa-desktop/src-tauri/src/credentials.rs",
    ]
    .iter()
    .any(|prefix| p == *prefix || p.starts_with(&format!("{prefix}/")))
}
fn fingerprint(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}
fn coordinator_error(error: CoordinatorError) -> MedusaError {
    MedusaError::new(
        ErrorCode::InternalInvariant,
        ErrorCategory::Internal,
        error.to_string(),
    )
}
fn invalid(message: impl Into<String>) -> MedusaError {
    MedusaError::new(ErrorCode::InvalidInput, ErrorCategory::Validation, message)
}
fn policy_denied(message: impl Into<String>) -> MedusaError {
    MedusaError::new(ErrorCode::PolicyDenied, ErrorCategory::Policy, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use medusa_github::CommandOutput;
    use medusa_improvement::{ImprovementRisk, ImprovementTarget};
    use std::{
        path::PathBuf,
        sync::{Arc, Mutex},
    };
    type RecordedCommands = Arc<Mutex<Vec<(String, Vec<String>)>>>;
    #[derive(Clone, Default)]
    struct FakeExecutor(RecordedCommands);
    impl CommandExecutor for FakeExecutor {
        fn run(
            &self,
            program: &str,
            arguments: &[String],
            _: Option<&Path>,
        ) -> MedusaResult<CommandOutput> {
            self.0
                .lock()
                .expect("commands")
                .push((program.into(), arguments.to_vec()));
            Ok(CommandOutput {
                success: true,
                stdout: "ok".into(),
                stderr: String::new(),
            })
        }
    }
    struct FakeProbe;
    impl crate::CommandProbe for FakeProbe {
        fn available(&self, program: &str, _: &[&str]) -> bool {
            matches!(program, "gh" | "git" | "node" | "sh" | "cmd")
        }
    }
    fn runtime() -> ExplicitCapabilityRuntime<FakeExecutor> {
        let directory = tempfile::tempdir().expect("tempdir").keep();
        let registry =
            CapabilityRegistry::discover_with(directory.clone(), &FakeProbe).expect("registry");
        let grants = vec![
            CapabilityGrant {
                capability: Capability::GitHub,
                permissions: BTreeSet::from([
                    CapabilityPermission::Read,
                    CapabilityPermission::Write,
                ]),
            },
            CapabilityGrant {
                capability: Capability::SelfImprovement,
                permissions: BTreeSet::from([
                    CapabilityPermission::Read,
                    CapabilityPermission::RepositoryMutation,
                ]),
            },
        ];
        ExplicitCapabilityRuntime::new(
            registry,
            grants,
            GitHubService::with_executor(
                "owner/repo",
                "github.com",
                Some(directory),
                FakeExecutor::default(),
            ),
        )
    }
    fn patch(id: &str, path: &str) -> ImprovementPatchProposal {
        ImprovementPatchProposal {
            proposal: ImprovementProposal {
                id: id.into(),
                target: ImprovementTarget::Skill,
                risk: ImprovementRisk::Low,
                source_sessions: vec!["session-1".into()],
                problem: "Repeated verification friction".into(),
                evidence: vec!["failure-1".into()],
                proposed_change: "Improve verification guidance".into(),
                rejected_alternatives: vec!["No change".into()],
                evaluation_plan: "Run focused tests".into(),
                safety_analysis: "Transactional mutation".into(),
                rollback: "Restore snapshot".into(),
                touched_paths: BTreeSet::from([PathBuf::from(path)]),
            },
            rationale: "Evidence supports a scoped update".into(),
            unified_diff: format!("--- a/{path}\n+++ b/{path}"),
            verification_commands: vec!["cargo test -p medusa-capabilities".into()],
            rollback_metadata: "restore pre-transaction tree".into(),
        }
    }
    fn verified(_: &ImprovementPatchProposal) -> MedusaResult<VerificationEvidence> {
        Ok(VerificationEvidence {
            command_results: vec!["passed".into()],
        })
    }
    #[test]
    fn diff_paths_must_match_declared_paths() {
        let mut p = patch("one", "skills/x.md");
        p.unified_diff =
            "--- a/crates/medusa-agent/src/policy.rs\n+++ b/crates/medusa-agent/src/policy.rs"
                .into();
        assert!(p.validate().is_err());
    }
    #[test]
    fn duplicate_improvement_ids_are_rejected() {
        let mut r = runtime();
        r.stage_improvement(patch("same", "skills/x.md"), false)
            .expect("first");
        assert!(
            r.stage_improvement(patch("same", "skills/x.md"), false)
                .is_err()
        );
    }
    #[test]
    fn verification_is_required_before_commit() {
        let mut r = runtime();
        let id = r
            .stage_improvement(patch("verify", "skills/x.md"), false)
            .expect("stage");
        let mut committed = false;
        assert!(
            r.approve_improvement(
                &id,
                true,
                |_| Err(invalid("tests failed")),
                |_| {
                    committed = true;
                    Ok(())
                },
                |_| Ok(())
            )
            .is_err()
        );
        assert!(!committed);
    }
    #[test]
    fn commit_failure_executes_rollback() {
        let mut r = runtime();
        let id = r
            .stage_improvement(patch("rollback", "skills/x.md"), false)
            .expect("stage");
        let mut rolled_back = false;
        assert!(
            r.approve_improvement(
                &id,
                true,
                verified,
                |_| Err(invalid("partial mutation")),
                |_| {
                    rolled_back = true;
                    Ok(())
                }
            )
            .is_err()
        );
        assert!(rolled_back);
        assert_eq!(r.transaction_phase(&id), Some(TransactionPhase::Failed));
    }
    #[test]
    fn sensitive_implementations_require_separate_approval() {
        let mut r = runtime();
        assert!(
            r.stage_improvement(
                patch("sensitive", "crates/medusa-agent/src/policy.rs"),
                false
            )
            .is_err()
        );
    }
    #[test]
    fn approved_verified_improvement_commits() {
        let mut r = runtime();
        let id = r
            .stage_improvement(patch("ok", "skills/x.md"), false)
            .expect("stage");
        r.approve_improvement(&id, true, verified, |_| Ok(()), |_| Ok(()))
            .expect("commit");
        assert_eq!(r.transaction_phase(&id), Some(TransactionPhase::Committed));
    }
}
