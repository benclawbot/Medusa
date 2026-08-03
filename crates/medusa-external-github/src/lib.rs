//! Typed GitHub operation adapter with normalized attempt-bound receipts.
//!
//! The existing `medusa-github` service remains the command-construction and credential-store
//! boundary. This crate is the external-operation authority layered above it: callers submit a
//! versioned [`OperationEnvelope`], receive a normalized receipt for both success and failure,
//! and cannot silently retry an uncertain non-idempotent create.

use std::path::PathBuf;

use medusa_core::{ErrorCategory, ErrorCode, MedusaError};
use medusa_external_contracts::{
    ContractError, ExternalOperation, ExternalTransport, OperationEnvelope, OperationLifecycle,
    OperationReceipt, ReconciliationState, TrustedHost,
};
use medusa_github::{CommandExecutor, GitHubService, MergeStrategy, SystemExecutor};

/// Successful typed GitHub execution.
#[derive(Clone, Debug, PartialEq)]
pub struct GitHubOperationOutcome {
    pub receipt: OperationReceipt,
    pub output: String,
}

/// Failed typed GitHub execution, including the durable normalized receipt candidate.
#[derive(Clone, Debug)]
pub struct GitHubOperationFailure {
    pub receipt: OperationReceipt,
    pub error: MedusaError,
}

/// GitHub operation authority backed by the existing shell-free `gh` command adapter.
#[derive(Clone, Debug)]
pub struct GitHubOperationService<E = SystemExecutor> {
    service: GitHubService<E>,
    repository: String,
    hostname: String,
}

impl GitHubOperationService<SystemExecutor> {
    #[must_use]
    pub fn new(repository: impl Into<String>) -> Self {
        Self::with_executor(repository, "github.com", None, SystemExecutor)
    }
}

impl<E: CommandExecutor> GitHubOperationService<E> {
    #[must_use]
    pub fn with_executor(
        repository: impl Into<String>,
        hostname: impl Into<String>,
        directory: Option<PathBuf>,
        executor: E,
    ) -> Self {
        let repository = repository.into();
        let hostname = hostname.into();
        Self {
            service: GitHubService::enterprise(
                repository.clone(),
                hostname.clone(),
                directory,
                executor,
            ),
            repository,
            hostname,
        }
    }

    /// Executes one typed operation and returns an attempt-bound receipt on every path.
    pub fn execute(
        &self,
        envelope: &OperationEnvelope,
    ) -> Result<GitHubOperationOutcome, GitHubOperationFailure> {
        if let Err(error) = envelope.validate() {
            return Err(self.failure(
                envelope,
                OperationLifecycle::Failed,
                ReconciliationState::NotRequired,
                contract_error(error),
            ));
        }
        if let Err(error) = self.validate_boundary(envelope) {
            return Err(self.failure(
                envelope,
                OperationLifecycle::Failed,
                ReconciliationState::NotRequired,
                error,
            ));
        }
        match self.service.auth_status() {
            Ok(status) if status.authenticated => {}
            Ok(_) => {
                return Err(self.failure(
                    envelope,
                    OperationLifecycle::Failed,
                    ReconciliationState::NotRequired,
                    MedusaError::new(
                        ErrorCode::PolicyDenied,
                        ErrorCategory::Policy,
                        format!("GitHub CLI is not authenticated for {}", self.hostname),
                    ),
                ));
            }
            Err(error) => {
                return Err(self.failure(
                    envelope,
                    OperationLifecycle::Failed,
                    ReconciliationState::NotRequired,
                    error,
                ));
            }
        }

        match self.dispatch(&envelope.operation) {
            Ok(output) => {
                let mut receipt = self.receipt(envelope, OperationLifecycle::Persisted);
                receipt.persisted = true;
                if output.starts_with("https://") || output.starts_with("http://") {
                    receipt.resource_id = output
                        .trim_end_matches('/')
                        .rsplit('/')
                        .next()
                        .map(str::to_owned);
                    receipt.resource_url = Some(output.clone());
                }
                if let Err(error) = receipt.validate_against(envelope) {
                    return Err(self.failure(
                        envelope,
                        OperationLifecycle::Failed,
                        ReconciliationState::NotRequired,
                        contract_error(error),
                    ));
                }
                Ok(GitHubOperationOutcome { receipt, output })
            }
            Err(error) => {
                let uncertain = envelope.operation.is_non_idempotent_create()
                    && (error.retryable || error.category == ErrorCategory::Transient);
                Err(self.failure(
                    envelope,
                    if uncertain {
                        OperationLifecycle::Uncertain
                    } else {
                        OperationLifecycle::Failed
                    },
                    if uncertain {
                        ReconciliationState::Pending
                    } else {
                        ReconciliationState::NotRequired
                    },
                    error,
                ))
            }
        }
    }

    fn validate_boundary(&self, envelope: &OperationEnvelope) -> Result<(), MedusaError> {
        let trusted =
            TrustedHost::parse(&format!("https://{}", self.hostname)).map_err(contract_error)?;
        if !trusted
            .permits(&format!("https://{}/", self.hostname))
            .map_err(contract_error)?
        {
            return Err(MedusaError::new(
                ErrorCode::PolicyDenied,
                ErrorCategory::Policy,
                "GitHub credential host does not match the configured operation host",
            ));
        }
        if envelope.operation.is_non_idempotent_create() && envelope.idempotency_key.is_none() {
            return Err(MedusaError::new(
                ErrorCode::InvalidInput,
                ErrorCategory::Validation,
                "non-idempotent GitHub creates require an idempotency key",
            ));
        }
        match &envelope.operation {
            ExternalOperation::RepositoryMetadata { repository }
            | ExternalOperation::IssueCreate { repository, .. }
            | ExternalOperation::PullRequestMerge { repository, .. }
            | ExternalOperation::WorkflowRerun { repository, .. }
            | ExternalOperation::ArtifactDownload { repository, .. }
                if repository != &self.repository =>
            {
                Err(MedusaError::new(
                    ErrorCode::PolicyDenied,
                    ErrorCategory::Policy,
                    format!(
                        "operation repository {repository} does not match bound repository {}",
                        self.repository
                    ),
                ))
            }
            _ => Ok(()),
        }
    }

    fn dispatch(&self, operation: &ExternalOperation) -> Result<String, MedusaError> {
        match operation {
            ExternalOperation::IssueCreate { title, body, .. } => {
                self.service.create_issue(title, body)
            }
            ExternalOperation::PullRequestMerge {
                number,
                strategy,
                expected_head,
                ..
            } => {
                if expected_head.is_some() {
                    return Err(MedusaError::new(
                        ErrorCode::InvalidInput,
                        ErrorCategory::Validation,
                        "the GitHub CLI backend cannot enforce expected_head; use a direct API backend",
                    ));
                }
                self.service
                    .merge_pr(*number, parse_merge_strategy(strategy)?)
            }
            ExternalOperation::WorkflowRerun {
                run_id,
                failed_jobs_only: true,
                ..
            } => self.service.rerun_failed_jobs(*run_id),
            ExternalOperation::WorkflowRerun {
                failed_jobs_only: false,
                ..
            } => Err(MedusaError::new(
                ErrorCode::InvalidInput,
                ErrorCategory::Validation,
                "the current GitHub CLI backend exposes failed-job reruns only",
            )),
            ExternalOperation::RepositoryMetadata { .. }
            | ExternalOperation::RepositoryCreate { .. }
            | ExternalOperation::ArtifactDownload { .. }
            | ExternalOperation::Custom { .. } => Err(MedusaError::new(
                ErrorCode::DependencyUnavailable,
                ErrorCategory::Environment,
                "typed operation is not implemented by the GitHub CLI backend",
            )),
        }
    }

    fn receipt(
        &self,
        envelope: &OperationEnvelope,
        lifecycle: OperationLifecycle,
    ) -> OperationReceipt {
        OperationReceipt::for_envelope(
            envelope,
            ExternalTransport::GitHubCli,
            "gh",
            self.hostname.clone(),
            lifecycle,
        )
    }

    fn failure(
        &self,
        envelope: &OperationEnvelope,
        lifecycle: OperationLifecycle,
        reconciliation: ReconciliationState,
        error: MedusaError,
    ) -> GitHubOperationFailure {
        let mut receipt = self.receipt(envelope, lifecycle);
        receipt.reconciliation = reconciliation;
        receipt.metadata.insert(
            "error_code".to_owned(),
            serde_json::Value::String(format!("{:?}", error.code)),
        );
        receipt.metadata.insert(
            "error_category".to_owned(),
            serde_json::Value::String(format!("{:?}", error.category)),
        );
        GitHubOperationFailure { receipt, error }
    }
}

fn parse_merge_strategy(strategy: &str) -> Result<MergeStrategy, MedusaError> {
    match strategy {
        "merge" => Ok(MergeStrategy::Merge),
        "squash" => Ok(MergeStrategy::Squash),
        "rebase" => Ok(MergeStrategy::Rebase),
        other => Err(MedusaError::new(
            ErrorCode::InvalidInput,
            ErrorCategory::Validation,
            format!("unsupported pull-request merge strategy {other}"),
        )),
    }
}

fn contract_error(error: ContractError) -> MedusaError {
    MedusaError::new(
        ErrorCode::InvalidInput,
        ErrorCategory::Validation,
        error.to_string(),
    )
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use medusa_external_contracts::{
        ExternalOperation, IdempotencyKey, OperationEnvelope, OperationLifecycle,
        ReconciliationState,
    };
    use medusa_github::{CommandExecutor, CommandOutput};

    use super::*;

    #[derive(Clone, Default)]
    struct FakeExecutor {
        calls: Arc<Mutex<Vec<Vec<String>>>>,
        transient_issue_error: bool,
    }

    impl CommandExecutor for FakeExecutor {
        fn run(
            &self,
            _program: &str,
            arguments: &[String],
            _directory: Option<&std::path::Path>,
        ) -> medusa_core::MedusaResult<CommandOutput> {
            self.calls.lock().unwrap().push(arguments.to_vec());
            let auth = arguments.starts_with(&["auth".to_owned(), "status".to_owned()]);
            let issue = arguments.starts_with(&["issue".to_owned(), "create".to_owned()]);
            if issue && self.transient_issue_error {
                return Err(MedusaError::new(
                    ErrorCode::DependencyUnavailable,
                    ErrorCategory::Transient,
                    "simulated uncertain GitHub create",
                )
                .with_retryable(true));
            }
            Ok(CommandOutput {
                success: true,
                stdout: if issue {
                    "https://github.com/octo/repo/issues/7".to_owned()
                } else if auth {
                    "authenticated".to_owned()
                } else {
                    String::new()
                },
                stderr: String::new(),
            })
        }
    }

    fn issue_envelope(key: Option<IdempotencyKey>) -> OperationEnvelope {
        OperationEnvelope::new(
            ExternalOperation::IssueCreate {
                repository: "octo/repo".to_owned(),
                title: "Typed issue".to_owned(),
                body: "Body".to_owned(),
            },
            key,
        )
        .unwrap()
    }

    #[test]
    fn issue_create_returns_persisted_attempt_bound_receipt() {
        let service = GitHubOperationService::with_executor(
            "octo/repo",
            "github.com",
            None,
            FakeExecutor::default(),
        );
        let envelope = issue_envelope(Some(IdempotencyKey::parse("issue-create-7").unwrap()));
        let outcome = service.execute(&envelope).unwrap();
        assert_eq!(outcome.receipt.lifecycle, OperationLifecycle::Persisted);
        assert!(outcome.receipt.persisted);
        assert_eq!(outcome.receipt.attempt_id, envelope.attempt_id);
        assert_eq!(outcome.receipt.request_digest, envelope.request_digest);
        assert_eq!(outcome.receipt.resource_id.as_deref(), Some("7"));
        outcome.receipt.validate_against(&envelope).unwrap();
    }

    #[test]
    fn create_requires_idempotency_key_before_dispatch() {
        let executor = FakeExecutor::default();
        let calls = Arc::clone(&executor.calls);
        let service =
            GitHubOperationService::with_executor("octo/repo", "github.com", None, executor);
        let failure = service.execute(&issue_envelope(None)).unwrap_err();
        assert_eq!(failure.receipt.lifecycle, OperationLifecycle::Failed);
        assert!(calls.lock().unwrap().is_empty());
    }

    #[test]
    fn transient_create_failure_requires_reconciliation() {
        let service = GitHubOperationService::with_executor(
            "octo/repo",
            "github.com",
            None,
            FakeExecutor {
                transient_issue_error: true,
                ..FakeExecutor::default()
            },
        );
        let envelope = issue_envelope(Some(IdempotencyKey::parse("issue-create-8").unwrap()));
        let failure = service.execute(&envelope).unwrap_err();
        assert_eq!(failure.receipt.lifecycle, OperationLifecycle::Uncertain);
        assert_eq!(failure.receipt.reconciliation, ReconciliationState::Pending);
        failure.receipt.validate_against(&envelope).unwrap();
    }

    #[test]
    fn mismatched_repository_is_rejected_before_authentication() {
        let executor = FakeExecutor::default();
        let calls = Arc::clone(&executor.calls);
        let service =
            GitHubOperationService::with_executor("octo/other", "github.com", None, executor);
        let failure = service
            .execute(&issue_envelope(Some(
                IdempotencyKey::parse("issue-create-9").unwrap(),
            )))
            .unwrap_err();
        assert_eq!(
            failure.receipt.reconciliation,
            ReconciliationState::NotRequired
        );
        assert!(calls.lock().unwrap().is_empty());
    }
}
