use std::{collections::BTreeSet, path::Path};

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use medusa_github::{CommandExecutor, GitHubService};
use medusa_improvement::ImprovementProposal;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProductCapability {
    GitHubOperations,
    SelfImprovement,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityPermission {
    GitHubRead,
    GitHubWrite,
    RepositoryRead,
    RepositoryWrite,
    RunVerification,
    CommitChanges,
    RollbackChanges,
    ExplicitApproval,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CapabilityDescriptor {
    pub capability: ProductCapability,
    pub label: &'static str,
    pub configuration: &'static str,
    pub required_permissions: BTreeSet<CapabilityPermission>,
    pub audit_events: Vec<&'static str>,
}

impl CapabilityDescriptor {
    #[must_use]
    pub fn github() -> Self {
        Self {
            capability: ProductCapability::GitHubOperations,
            label: "GitHub operations",
            configuration: "GitHub CLI authentication and repository identity",
            required_permissions: BTreeSet::from([
                CapabilityPermission::GitHubRead,
                CapabilityPermission::RepositoryRead,
            ]),
            audit_events: vec![
                "github.read.requested",
                "github.write.proposed",
                "github.write.approved",
                "github.operation.completed",
                "github.operation.denied",
            ],
        }
    }

    #[must_use]
    pub fn self_improvement() -> Self {
        Self {
            capability: ProductCapability::SelfImprovement,
            label: "Self-improvement",
            configuration: "reviewable proposal transaction with verification and rollback",
            required_permissions: BTreeSet::from([
                CapabilityPermission::RepositoryRead,
                CapabilityPermission::RunVerification,
                CapabilityPermission::ExplicitApproval,
            ]),
            audit_events: vec![
                "improvement.proposed",
                "improvement.denied",
                "improvement.approved",
                "improvement.verified",
                "improvement.committed",
                "improvement.rolled_back",
            ],
        }
    }

    #[must_use]
    pub fn all() -> [Self; 2] {
        [Self::github(), Self::self_improvement()]
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AuditRecord {
    pub event: String,
    pub capability: ProductCapability,
    pub approved: bool,
    pub detail: String,
}

fn require(
    granted: &BTreeSet<CapabilityPermission>,
    permission: CapabilityPermission,
) -> MedusaResult<()> {
    if granted.contains(&permission) {
        Ok(())
    } else {
        Err(MedusaError::new(
            ErrorCode::PolicyDenied,
            ErrorCategory::Policy,
            format!("capability permission {permission:?} is required before side effects"),
        ))
    }
}

pub struct GitHubCapability<E> {
    service: GitHubService<E>,
    granted: BTreeSet<CapabilityPermission>,
    audit: Vec<AuditRecord>,
}

impl<E: CommandExecutor> GitHubCapability<E> {
    #[must_use]
    pub fn new(
        service: GitHubService<E>,
        granted: BTreeSet<CapabilityPermission>,
    ) -> Self {
        Self {
            service,
            granted,
            audit: Vec::new(),
        }
    }

    pub fn auth_status(&mut self) -> MedusaResult<medusa_github::AuthStatus> {
        require(&self.granted, CapabilityPermission::GitHubRead)?;
        let result = self.service.auth_status();
        self.audit.push(AuditRecord {
            event: "github.read.requested".into(),
            capability: ProductCapability::GitHubOperations,
            approved: result.is_ok(),
            detail: "read GitHub authentication status".into(),
        });
        result
    }

    pub fn create_issue(&mut self, title: &str, body: &str) -> MedusaResult<String> {
        require(&self.granted, CapabilityPermission::GitHubWrite)?;
        require(&self.granted, CapabilityPermission::ExplicitApproval)?;
        self.audit.push(AuditRecord {
            event: "github.write.approved".into(),
            capability: ProductCapability::GitHubOperations,
            approved: true,
            detail: format!("create issue: {title}"),
        });
        self.service.create_issue(title, body)
    }

    #[must_use]
    pub fn audit(&self) -> &[AuditRecord] {
        &self.audit
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TransactionState {
    Proposed,
    Approved,
    Verified,
    Committed,
    RolledBack,
}

pub struct SelfImprovementTransaction {
    proposal: ImprovementProposal,
    granted: BTreeSet<CapabilityPermission>,
    state: TransactionState,
    audit: Vec<AuditRecord>,
}

impl SelfImprovementTransaction {
    pub fn propose(
        proposal: ImprovementProposal,
        granted: BTreeSet<CapabilityPermission>,
    ) -> MedusaResult<Self> {
        proposal.validate()?;
        reject_protected_paths(&proposal)?;
        Ok(Self {
            proposal,
            granted,
            state: TransactionState::Proposed,
            audit: vec![AuditRecord {
                event: "improvement.proposed".into(),
                capability: ProductCapability::SelfImprovement,
                approved: false,
                detail: "reviewable improvement proposal created".into(),
            }],
        })
    }

    pub fn approve(&mut self) -> MedusaResult<()> {
        require(&self.granted, CapabilityPermission::ExplicitApproval)?;
        require(&self.granted, CapabilityPermission::RepositoryWrite)?;
        self.state = TransactionState::Approved;
        self.audit.push(AuditRecord {
            event: "improvement.approved".into(),
            capability: ProductCapability::SelfImprovement,
            approved: true,
            detail: self.proposal.id.clone(),
        });
        Ok(())
    }

    pub fn mark_verified(&mut self) -> MedusaResult<()> {
        require(&self.granted, CapabilityPermission::RunVerification)?;
        if self.state != TransactionState::Approved {
            return Err(invalid_transition("verification requires approval"));
        }
        self.state = TransactionState::Verified;
        self.audit.push(AuditRecord {
            event: "improvement.verified".into(),
            capability: ProductCapability::SelfImprovement,
            approved: true,
            detail: self.proposal.evaluation_plan.clone(),
        });
        Ok(())
    }

    pub fn commit(&mut self) -> MedusaResult<()> {
        require(&self.granted, CapabilityPermission::CommitChanges)?;
        if self.state != TransactionState::Verified {
            return Err(invalid_transition("commit requires verified state"));
        }
        self.state = TransactionState::Committed;
        self.audit.push(AuditRecord {
            event: "improvement.committed".into(),
            capability: ProductCapability::SelfImprovement,
            approved: true,
            detail: self.proposal.id.clone(),
        });
        Ok(())
    }

    pub fn rollback(&mut self) -> MedusaResult<()> {
        require(&self.granted, CapabilityPermission::RollbackChanges)?;
        self.state = TransactionState::RolledBack;
        self.audit.push(AuditRecord {
            event: "improvement.rolled_back".into(),
            capability: ProductCapability::SelfImprovement,
            approved: true,
            detail: self.proposal.rollback.clone(),
        });
        Ok(())
    }

    #[must_use]
    pub fn state(&self) -> TransactionState {
        self.state
    }

    #[must_use]
    pub fn audit(&self) -> &[AuditRecord] {
        &self.audit
    }
}

fn reject_protected_paths(proposal: &ImprovementProposal) -> MedusaResult<()> {
    const PROTECTED: [&str; 6] = [
        "policy",
        "sandbox",
        "approval",
        "credential",
        "update-trust",
        ".github/workflows",
    ];
    if proposal.touched_paths.iter().any(|path| {
        PROTECTED
            .iter()
            .any(|protected| path_starts_with_component(path, protected))
    }) {
        return Err(MedusaError::new(
            ErrorCode::PolicyDenied,
            ErrorCategory::Policy,
            "self-improvement cannot modify policy, sandbox, approval, credential, workflow, or update-trust controls",
        ));
    }
    Ok(())
}

fn path_starts_with_component(path: &Path, prefix: &str) -> bool {
    path.starts_with(prefix)
        || path
            .to_string_lossy()
            .replace('\\', "/")
            .starts_with(prefix)
}

fn invalid_transition(message: &str) -> MedusaError {
    MedusaError::new(
        ErrorCode::PolicyDenied,
        ErrorCategory::Policy,
        message,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use medusa_github::CommandOutput;
    use medusa_improvement::{ImprovementRisk, ImprovementTarget};
    use std::{path::PathBuf, sync::{Arc, Mutex}};

    #[derive(Clone, Default)]
    struct FakeExecutor(Arc<Mutex<Vec<(String, Vec<String>)>>>);

    impl CommandExecutor for FakeExecutor {
        fn run(
            &self,
            program: &str,
            arguments: &[String],
            _: Option<&Path>,
        ) -> MedusaResult<CommandOutput> {
            self.0.lock().expect("commands").push((program.into(), arguments.to_vec()));
            Ok(CommandOutput {
                success: true,
                stdout: "ok".into(),
                stderr: String::new(),
            })
        }
    }

    fn proposal(paths: &[&str]) -> ImprovementProposal {
        ImprovementProposal {
            id: "proposal-1".into(),
            target: ImprovementTarget::Memory,
            risk: ImprovementRisk::Low,
            source_sessions: vec!["session-1".into()],
            problem: "repeated context miss".into(),
            evidence: vec!["three identical misses".into()],
            proposed_change: "add scoped memory guidance".into(),
            rejected_alternatives: Vec::new(),
            evaluation_plan: "run frozen memory retrieval suite".into(),
            safety_analysis: "no executable behavior".into(),
            rollback: "restore previous memory file".into(),
            touched_paths: paths.iter().map(PathBuf::from).collect(),
        }
    }

    #[test]
    fn denied_github_write_fails_before_executor_side_effect() {
        let executor = FakeExecutor::default();
        let recorded = executor.0.clone();
        let service = GitHubService::with_executor("owner/repo", "github.com", None, executor);
        let mut capability = GitHubCapability::new(
            service,
            BTreeSet::from([CapabilityPermission::GitHubRead]),
        );
        assert!(capability.create_issue("title", "body").is_err());
        assert!(recorded.lock().expect("commands").is_empty());
    }

    #[test]
    fn approved_github_write_routes_through_medusa_github() {
        let executor = FakeExecutor::default();
        let recorded = executor.0.clone();
        let service = GitHubService::with_executor("owner/repo", "github.com", None, executor);
        let mut capability = GitHubCapability::new(
            service,
            BTreeSet::from([
                CapabilityPermission::GitHubWrite,
                CapabilityPermission::ExplicitApproval,
            ]),
        );
        capability.create_issue("title", "body").expect("create issue");
        let commands = recorded.lock().expect("commands");
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0].0, "gh");
        assert!(commands[0].1.windows(2).any(|args| args == ["issue", "create"]));
    }

    #[test]
    fn improvement_must_follow_approval_verification_commit_barrier() {
        let mut transaction = SelfImprovementTransaction::propose(
            proposal(&["memory/rules.md"]),
            BTreeSet::from([
                CapabilityPermission::ExplicitApproval,
                CapabilityPermission::RepositoryWrite,
                CapabilityPermission::RunVerification,
                CapabilityPermission::CommitChanges,
                CapabilityPermission::RollbackChanges,
            ]),
        )
        .expect("proposal");
        assert!(transaction.commit().is_err());
        transaction.approve().expect("approve");
        transaction.mark_verified().expect("verify");
        transaction.commit().expect("commit");
        assert_eq!(transaction.state(), TransactionState::Committed);
    }

    #[test]
    fn protected_controls_are_never_self_modified() {
        for path in [
            "policy/network.toml",
            "sandbox/profile.json",
            "approval/rules.toml",
            "credential/store.json",
            "update-trust/roots.json",
            ".github/workflows/release.yml",
        ] {
            assert!(SelfImprovementTransaction::propose(
                proposal(&[path]),
                BTreeSet::new(),
            )
            .is_err(), "{path} must be protected");
        }
    }

    #[test]
    fn descriptors_are_visible_to_frontends_and_diagnostics() {
        let descriptors = CapabilityDescriptor::all();
        assert_eq!(descriptors.len(), 2);
        assert!(descriptors.iter().any(|descriptor| {
            descriptor.capability == ProductCapability::GitHubOperations
                && descriptor.audit_events.contains(&"github.write.approved")
        }));
        assert!(descriptors.iter().any(|descriptor| {
            descriptor.capability == ProductCapability::SelfImprovement
                && descriptor.required_permissions.contains(&CapabilityPermission::ExplicitApproval)
        }));
    }
}
