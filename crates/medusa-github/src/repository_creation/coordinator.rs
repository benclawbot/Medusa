use std::{collections::BTreeMap, path::PathBuf};

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use serde_json::{Map, Value, json};

use super::*;
use crate::CommandExecutor;

pub trait DirectGitHubOperationExecutor {
    fn capabilities(
        &self,
        document: &GitHubOperationDocument,
    ) -> MedusaResult<GitHubCapabilityReport>;
    fn execute(
        &self,
        document: &GitHubOperationDocument,
        prepared: &PreparedGitHubOperation,
    ) -> MedusaResult<GitHubBackendExecution>;
}

impl<S, O, H> DirectGitHubOperationExecutor for DirectGitHubBackend<S, O, H>
where
    S: GitHubCredentialStore,
    O: GitHubOAuthTransport,
    H: GitHubApiTransport,
{
    fn capabilities(
        &self,
        document: &GitHubOperationDocument,
    ) -> MedusaResult<GitHubCapabilityReport> {
        DirectGitHubBackend::capabilities(self, document)
    }

    fn execute(
        &self,
        document: &GitHubOperationDocument,
        prepared: &PreparedGitHubOperation,
    ) -> MedusaResult<GitHubBackendExecution> {
        DirectGitHubBackend::execute(self, document, prepared)
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct NoDirectGitHubBackend;

impl DirectGitHubOperationExecutor for NoDirectGitHubBackend {
    fn capabilities(
        &self,
        document: &GitHubOperationDocument,
    ) -> MedusaResult<GitHubCapabilityReport> {
        Ok(GitHubCapabilityReport {
            backend: GitHubExecutionBackend::DirectOauth,
            available: false,
            authenticated: false,
            login: None,
            repository: document.repository.clone(),
            hostname: document.hostname.clone(),
            api_version: document.api_version.clone(),
            role_name: None,
            permissions: BTreeMap::new(),
            supported_operations: Vec::new(),
            missing_permissions: vec!["direct OAuth backend is not configured".into()],
            rate_limit: GitHubRateLimit::default(),
            artifact_download: false,
            release_asset_upload: false,
            credential_backend: "not configured".into(),
        })
    }

    fn execute(
        &self,
        _document: &GitHubOperationDocument,
        _prepared: &PreparedGitHubOperation,
    ) -> MedusaResult<GitHubBackendExecution> {
        Err(dependency_error(
            "direct OAuth backend is not configured for this coordinator",
        ))
    }
}

pub struct GitHubOperationCoordinator<E, D> {
    service: GitHubService<E>,
    direct: D,
    ledger: FileGitHubOperationLedger,
}

impl<E: CommandExecutor, D: DirectGitHubOperationExecutor> GitHubOperationCoordinator<E, D> {
    #[must_use]
    pub fn new(service: GitHubService<E>, direct: D, ledger: FileGitHubOperationLedger) -> Self {
        Self {
            service,
            direct,
            ledger,
        }
    }

    pub fn capabilities(
        &self,
        document: &GitHubOperationDocument,
    ) -> MedusaResult<GitHubCapabilityReport> {
        document.validate()?;
        self.require_scope(document)?;
        match document.backend {
            GitHubExecutionBackend::DirectOauth => self.direct.capabilities(document),
            GitHubExecutionBackend::NativeCli | GitHubExecutionBackend::GhApi => {
                self.gh_capabilities(document)
            }
        }
    }

    pub fn execute(
        &self,
        document: &GitHubOperationDocument,
    ) -> MedusaResult<GitHubOperationReceiptV2> {
        document.validate()?;
        self.require_scope(document)?;
        let prepared = document.prepare()?;
        let request_digest = document.request_digest()?;
        let key_hash = document.idempotency_key_hash();

        match self.ledger.lookup(key_hash.as_deref(), &request_digest)? {
            IdempotencyLookup::Replay(receipt) => return Ok(receipt),
            IdempotencyLookup::ResumeUncertain => {
                if let Some(receipt) = self.reconcile_existing(
                    document,
                    &prepared,
                    new_github_attempt_id(),
                    request_digest.clone(),
                )? {
                    return Ok(receipt);
                }
                if !prepared.idempotent {
                    return Err(uncertain_error(
                        "prior GitHub dispatch has an uncertain result and reconciliation did not prove absence; refusing to repeat a non-idempotent mutation",
                    ));
                }
            }
            IdempotencyLookup::Miss => {}
        }

        let attempt_id = new_github_attempt_id();
        self.ledger.append_lifecycle(
            document,
            &prepared,
            &attempt_id,
            &request_digest,
            GitHubOperationLifecycleStatus::Requested,
            None,
        )?;
        self.ledger.append_lifecycle(
            document,
            &prepared,
            &attempt_id,
            &request_digest,
            GitHubOperationLifecycleStatus::Authorized,
            None,
        )?;
        self.ledger.append_lifecycle(
            document,
            &prepared,
            &attempt_id,
            &request_digest,
            GitHubOperationLifecycleStatus::Dispatched,
            None,
        )?;

        let execution = match self.dispatch(document, &prepared) {
            Ok(execution) => execution,
            Err(error) => {
                if prepared.risk.requires_approval() && prepared.reconciliation.is_some() {
                    self.ledger.append_lifecycle(
                        document,
                        &prepared,
                        &attempt_id,
                        &request_digest,
                        GitHubOperationLifecycleStatus::Uncertain,
                        Some(error.message.clone()),
                    )?;
                    if let Some(receipt) =
                        self.reconcile_existing(document, &prepared, attempt_id, request_digest)?
                    {
                        return Ok(receipt);
                    }
                }
                self.ledger.append_lifecycle(
                    document,
                    &prepared,
                    &attempt_id,
                    &request_digest,
                    GitHubOperationLifecycleStatus::Failed,
                    Some(error.message.clone()),
                )?;
                return Err(error);
            }
        };

        self.ledger.append_lifecycle(
            document,
            &prepared,
            &attempt_id,
            &request_digest,
            GitHubOperationLifecycleStatus::Accepted,
            None,
        )?;
        let mut receipt = GitHubOperationReceiptV2::from_backend(
            document,
            &prepared,
            attempt_id.clone(),
            request_digest.clone(),
            execution,
        );
        receipt.reconciliation_status = if prepared.reconciliation.is_some() {
            GitHubReconciliationStatus::Confirmed
        } else {
            GitHubReconciliationStatus::NotRequired
        };
        self.ledger.append_lifecycle(
            document,
            &prepared,
            &attempt_id,
            &request_digest,
            GitHubOperationLifecycleStatus::Completed,
            None,
        )?;
        self.ledger.append_receipt(&receipt)?;
        receipt.audit_persisted = true;
        receipt.lifecycle_status = GitHubOperationLifecycleStatus::Persisted;
        self.ledger.append_receipt(&receipt)?;
        self.ledger.append_lifecycle(
            document,
            &prepared,
            &attempt_id,
            &request_digest,
            GitHubOperationLifecycleStatus::Persisted,
            None,
        )?;
        Ok(receipt)
    }

    fn require_scope(&self, document: &GitHubOperationDocument) -> MedusaResult<()> {
        if document.repository != self.service.repository
            || document.hostname != self.service.hostname
        {
            return Err(policy_error(
                "typed GitHub operation must match the coordinator repository and hostname",
            ));
        }
        Ok(())
    }

    fn dispatch(
        &self,
        document: &GitHubOperationDocument,
        prepared: &PreparedGitHubOperation,
    ) -> MedusaResult<GitHubBackendExecution> {
        match document.backend {
            GitHubExecutionBackend::DirectOauth => self.direct.execute(document, prepared),
            GitHubExecutionBackend::NativeCli | GitHubExecutionBackend::GhApi => {
                self.dispatch_gh(document, prepared)
            }
        }
    }

    fn dispatch_gh(
        &self,
        document: &GitHubOperationDocument,
        prepared: &PreparedGitHubOperation,
    ) -> MedusaResult<GitHubBackendExecution> {
        match &prepared.execution {
            PreparedExecution::Api(request) => {
                let mut request = request.clone();
                request.backend = match document.backend {
                    GitHubExecutionBackend::NativeCli => GitHubBackendKind::NativeCli,
                    GitHubExecutionBackend::GhApi | GitHubExecutionBackend::DirectOauth => {
                        GitHubBackendKind::RestApi
                    }
                };
                self.service
                    .execute_operation(&request)
                    .map(legacy_execution)
            }
            PreparedExecution::RepositoryCreate(request) => {
                let receipt = self.service.create_repository(request)?;
                Ok(GitHubBackendExecution {
                    transport: "gh_repository_create".into(),
                    mutated: receipt.created,
                    resource_identity: Some(receipt.repository.clone()),
                    resource_url: Some(receipt.web_url.clone()),
                    canonical: serde_json::to_value(&receipt).unwrap_or(Value::Null),
                    payload: serde_json::to_value(&receipt).unwrap_or(Value::Null),
                    truncated: false,
                    redacted: false,
                    http: GitHubHttpMetadata::default(),
                    retry_count: 0,
                    retry_reason: None,
                    artifact: None,
                })
            }
            PreparedExecution::ArtifactDownload(_) | PreparedExecution::ReleaseAssetUpload(_) => {
                Err(dependency_error(
                    "binary artifact transfers require the direct OAuth HTTPS backend",
                ))
            }
        }
    }

    fn gh_capabilities(
        &self,
        document: &GitHubOperationDocument,
    ) -> MedusaResult<GitHubCapabilityReport> {
        let auth = self.service.auth_status()?;
        if !auth.authenticated {
            return Ok(GitHubCapabilityReport {
                backend: document.backend,
                available: true,
                authenticated: false,
                login: None,
                repository: document.repository.clone(),
                hostname: document.hostname.clone(),
                api_version: document.api_version.clone(),
                role_name: None,
                permissions: BTreeMap::new(),
                supported_operations: supported_operation_groups(),
                missing_permissions: vec!["GitHub CLI authentication required".into()],
                rate_limit: GitHubRateLimit::default(),
                artifact_download: false,
                release_asset_upload: false,
                credential_backend: auth.credential_backend.into(),
            });
        }
        let request = GitHubOperationRequest {
            repository: document.repository.clone(),
            hostname: document.hostname.clone(),
            resource: GitHubResource::Repository,
            action: "capabilities".into(),
            method: GitHubHttpMethod::Get,
            endpoint: String::new(),
            query: BTreeMap::new(),
            body: None,
            backend: GitHubBackendKind::RestApi,
            paginate: false,
            max_response_bytes: document.max_response_bytes,
        };
        let receipt = self.service.execute_operation(&request)?;
        let permissions = receipt
            .payload
            .get("permissions")
            .and_then(Value::as_object)
            .map(|permissions| {
                permissions
                    .iter()
                    .filter_map(|(key, value)| value.as_bool().map(|value| (key.clone(), value)))
                    .collect()
            })
            .unwrap_or_default();
        let role_name = receipt
            .payload
            .get("role_name")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let required = permission_for_risk(document.risk()?);
        let missing_permissions = if permission_satisfies(&permissions, required) {
            Vec::new()
        } else {
            vec![format!("repository permission `{required}` is required")]
        };
        Ok(GitHubCapabilityReport {
            backend: document.backend,
            available: true,
            authenticated: true,
            login: None,
            repository: document.repository.clone(),
            hostname: document.hostname.clone(),
            api_version: document.api_version.clone(),
            role_name,
            permissions,
            supported_operations: supported_operation_groups(),
            missing_permissions,
            rate_limit: GitHubRateLimit::default(),
            artifact_download: false,
            release_asset_upload: false,
            credential_backend: auth.credential_backend.into(),
        })
    }

    fn reconcile_existing(
        &self,
        document: &GitHubOperationDocument,
        prepared: &PreparedGitHubOperation,
        attempt_id: String,
        request_digest: String,
    ) -> MedusaResult<Option<GitHubOperationReceiptV2>> {
        let Some(rule) = prepared.reconciliation.as_ref() else {
            return Ok(None);
        };
        let result = self.run_reconciliation(document, rule)?;
        let Some(execution) = result else {
            return Ok(None);
        };
        let mut receipt = GitHubOperationReceiptV2::from_backend(
            document,
            prepared,
            attempt_id.clone(),
            request_digest.clone(),
            execution,
        );
        receipt.lifecycle_status = GitHubOperationLifecycleStatus::Reconciled;
        receipt.reconciliation_status = GitHubReconciliationStatus::Confirmed;
        self.ledger.append_lifecycle(
            document,
            prepared,
            &attempt_id,
            &request_digest,
            GitHubOperationLifecycleStatus::Reconciled,
            None,
        )?;
        self.ledger.append_receipt(&receipt)?;
        receipt.audit_persisted = true;
        self.ledger.append_receipt(&receipt)?;
        Ok(Some(receipt))
    }

    fn run_reconciliation(
        &self,
        document: &GitHubOperationDocument,
        rule: &GitHubReconciliation,
    ) -> MedusaResult<Option<GitHubBackendExecution>> {
        let check = reconciliation_document(document, rule)?;
        let prepared = check.prepare()?;
        match self.dispatch(&check, &prepared) {
            Ok(mut execution) => {
                if reconciliation_matches(rule, &execution.payload) {
                    execution.transport = format!("{}_reconciliation", execution.transport);
                    execution.mutated = document.risk()?.requires_approval();
                    Ok(Some(execution))
                } else {
                    Ok(None)
                }
            }
            Err(error) if absent_error(&error) => match rule {
                GitHubReconciliation::ContentAbsent { .. }
                | GitHubReconciliation::ResourceAbsent { .. } => Ok(Some(absent_execution(rule))),
                _ => Ok(None),
            },
            Err(error) => Err(error),
        }
    }
}

fn legacy_execution(receipt: GitHubOperationReceipt) -> GitHubBackendExecution {
    GitHubBackendExecution {
        transport: receipt.transport,
        mutated: receipt.mutated,
        resource_identity: receipt.resource_identity,
        resource_url: receipt.resource_url,
        canonical: receipt.canonical,
        payload: receipt.payload,
        truncated: receipt.truncated,
        redacted: true,
        http: GitHubHttpMetadata::default(),
        retry_count: 0,
        retry_reason: None,
        artifact: None,
    }
}

fn reconciliation_document(
    original: &GitHubOperationDocument,
    rule: &GitHubReconciliation,
) -> MedusaResult<GitHubOperationDocument> {
    let operation = match rule {
        GitHubReconciliation::IssueMarker { marker } => {
            GitHubTypedOperation::Search(SearchOperation::Issues {
                query: format!("in:body \"{marker}\""),
                sort: Some("updated".into()),
                order: Some("desc".into()),
                per_page: 10,
            })
        }
        GitHubReconciliation::PullRequestMarker { marker } => {
            GitHubTypedOperation::Search(SearchOperation::Issues {
                query: format!("is:pr in:body \"{marker}\""),
                sort: Some("updated".into()),
                order: Some("desc".into()),
                per_page: 10,
            })
        }
        GitHubReconciliation::BranchExists { reference, .. } => {
            GitHubTypedOperation::GitData(GitDataOperation::GetBranch {
                branch: reference
                    .strip_prefix("refs/heads/")
                    .unwrap_or(reference)
                    .into(),
            })
        }
        GitHubReconciliation::ContentMatches { path, branch }
        | GitHubReconciliation::ContentAbsent { path, branch } => {
            GitHubTypedOperation::Contents(ContentsOperation::Get {
                path: path.clone(),
                reference: branch.clone(),
            })
        }
        GitHubReconciliation::ReleaseTag { tag } => {
            GitHubTypedOperation::Releases(ReleasesOperation::GetByTag { tag: tag.clone() })
        }
        GitHubReconciliation::ResourceAbsent { endpoint } => legacy_check_operation(endpoint)?,
    };
    Ok(GitHubOperationDocument {
        schema_version: GITHUB_OPERATION_SCHEMA_VERSION,
        repository: original.repository.clone(),
        hostname: original.hostname.clone(),
        api_base_url: original.api_base_url.clone(),
        oauth_base_url: original.oauth_base_url.clone(),
        api_version: original.api_version.clone(),
        backend: original.backend,
        idempotency_key: None,
        max_response_bytes: original.max_response_bytes,
        operation,
    })
}

fn legacy_check_operation(endpoint: &str) -> MedusaResult<GitHubTypedOperation> {
    if endpoint.is_empty() {
        return Ok(GitHubTypedOperation::Repository(RepositoryOperation::Get));
    }
    if let Some(value) = endpoint.strip_prefix("issues/comments/") {
        let comment_id = value
            .parse()
            .map_err(|_| invalid("invalid comment reconciliation ID"))?;
        return Ok(GitHubTypedOperation::Issues(IssuesOperation::Get {
            number: comment_id,
        }));
    }
    if let Some(value) = endpoint.strip_prefix("releases/assets/") {
        let asset_id = value
            .parse()
            .map_err(|_| invalid("invalid asset reconciliation ID"))?;
        return Ok(GitHubTypedOperation::Releases(ReleasesOperation::Get {
            release_id: asset_id,
        }));
    }
    if let Some(value) = endpoint.strip_prefix("releases/") {
        let release_id = value
            .parse()
            .map_err(|_| invalid("invalid release reconciliation ID"))?;
        return Ok(GitHubTypedOperation::Releases(ReleasesOperation::Get {
            release_id,
        }));
    }
    Err(invalid(
        "unsupported resource-absence reconciliation endpoint",
    ))
}

fn reconciliation_matches(rule: &GitHubReconciliation, payload: &Value) -> bool {
    match rule {
        GitHubReconciliation::IssueMarker { marker }
        | GitHubReconciliation::PullRequestMarker { marker } => payload
            .get("items")
            .and_then(Value::as_array)
            .is_some_and(|items| {
                items.iter().any(|item| {
                    item.get("body")
                        .and_then(Value::as_str)
                        .is_some_and(|body| body.contains(marker))
                })
            }),
        GitHubReconciliation::BranchExists { sha, .. } => payload
            .pointer("/commit/sha")
            .or_else(|| payload.get("sha"))
            .and_then(Value::as_str)
            .is_some_and(|value| value == sha),
        GitHubReconciliation::ContentMatches { .. } | GitHubReconciliation::ReleaseTag { .. } => {
            true
        }
        GitHubReconciliation::ContentAbsent { .. }
        | GitHubReconciliation::ResourceAbsent { .. } => false,
    }
}

fn absent_execution(rule: &GitHubReconciliation) -> GitHubBackendExecution {
    GitHubBackendExecution {
        transport: "reconciliation_absence".into(),
        mutated: true,
        resource_identity: None,
        resource_url: None,
        canonical: json!({"absent": true}),
        payload: json!({"absent": true, "rule": rule}),
        truncated: false,
        redacted: false,
        http: GitHubHttpMetadata {
            status: Some(404),
            ..GitHubHttpMetadata::default()
        },
        retry_count: 0,
        retry_reason: None,
        artifact: None,
    }
}

fn absent_error(error: &MedusaError) -> bool {
    error.message.contains("HTTP 404")
        || error.message.to_ascii_lowercase().contains("not found")
        || error
            .message
            .to_ascii_lowercase()
            .contains("could not resolve")
}

fn supported_operation_groups() -> Vec<String> {
    [
        "repository",
        "contents",
        "git_data",
        "issues",
        "pull_requests",
        "actions",
        "releases",
        "collaborators",
        "environments",
        "variables",
        "secrets",
        "webhooks",
        "branch_protection",
        "projects",
        "search",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect()
}

fn permission_for_risk(risk: GitHubOperationRisk) -> &'static str {
    match risk {
        GitHubOperationRisk::ReadOnly => "pull",
        GitHubOperationRisk::Mutation => "push",
        GitHubOperationRisk::Administration
        | GitHubOperationRisk::Secret
        | GitHubOperationRisk::Destructive => "admin",
    }
}

fn permission_satisfies(permissions: &BTreeMap<String, bool>, required: &str) -> bool {
    match required {
        "pull" => {
            permissions.get("pull").copied().unwrap_or(false)
                || permission_satisfies(permissions, "push")
        }
        "push" => {
            permissions.get("push").copied().unwrap_or(false)
                || permission_satisfies(permissions, "admin")
        }
        "admin" => permissions.get("admin").copied().unwrap_or(false),
        _ => false,
    }
}

fn invalid(message: impl Into<String>) -> MedusaError {
    MedusaError::new(ErrorCode::InvalidInput, ErrorCategory::Validation, message)
}

fn policy_error(message: impl Into<String>) -> MedusaError {
    MedusaError::new(ErrorCode::PolicyDenied, ErrorCategory::Policy, message)
}

fn dependency_error(message: impl Into<String>) -> MedusaError {
    MedusaError::new(
        ErrorCode::DependencyUnavailable,
        ErrorCategory::Environment,
        message,
    )
}

fn uncertain_error(message: impl Into<String>) -> MedusaError {
    MedusaError::new(
        ErrorCode::ToolExecutionFailed,
        ErrorCategory::Execution,
        message,
    )
}

#[cfg(test)]
mod tests {
    use std::{
        path::Path,
        sync::{Arc, Mutex},
    };

    use super::*;
    use crate::{CommandExecutor, CommandOutput};

    #[derive(Clone, Default)]
    struct FakeExecutor {
        commands: Arc<Mutex<Vec<(String, Vec<String>)>>>,
    }

    impl CommandExecutor for FakeExecutor {
        fn run(
            &self,
            program: &str,
            arguments: &[String],
            _directory: Option<&Path>,
        ) -> MedusaResult<CommandOutput> {
            self.commands
                .lock()
                .expect("commands")
                .push((program.into(), arguments.to_vec()));
            let output = if arguments.starts_with(&["auth".into(), "status".into()]) {
                CommandOutput {
                    success: true,
                    stdout: String::new(),
                    stderr: String::new(),
                }
            } else {
                CommandOutput {
                    success: true,
                    stdout: r#"{"full_name":"acme/project","html_url":"https://github.com/acme/project","permissions":{"pull":true,"push":true,"admin":true}}"#.into(),
                    stderr: String::new(),
                }
            };
            Ok(output)
        }
    }

    fn document(key: Option<&str>) -> GitHubOperationDocument {
        GitHubOperationDocument {
            schema_version: GITHUB_OPERATION_SCHEMA_VERSION,
            repository: "acme/project".into(),
            hostname: "github.com".into(),
            api_base_url: None,
            oauth_base_url: None,
            api_version: DEFAULT_GITHUB_API_VERSION.into(),
            backend: GitHubExecutionBackend::GhApi,
            idempotency_key: key.map(str::to_owned),
            max_response_bytes: 1024,
            operation: GitHubTypedOperation::Repository(RepositoryOperation::Get),
        }
    }

    #[test]
    fn exact_idempotent_execution_replays_without_second_dispatch() {
        let root = tempfile::tempdir().expect("root");
        let executor = FakeExecutor::default();
        let commands = executor.commands.clone();
        let service = GitHubService::enterprise(
            "acme/project",
            "github.com",
            Some(root.path().to_path_buf()),
            executor,
        );
        let coordinator = GitHubOperationCoordinator::new(
            service,
            NoDirectGitHubBackend,
            FileGitHubOperationLedger::at(root.path()).expect("ledger"),
        );
        let document = document(Some("read-repository"));
        let first = coordinator.execute(&document).expect("first");
        let command_count = commands.lock().expect("commands").len();
        let second = coordinator.execute(&document).expect("second");
        assert!(!first.replayed);
        assert!(second.replayed);
        assert_eq!(commands.lock().expect("commands").len(), command_count);
        assert_ne!(first.attempt_id, "");
    }

    #[test]
    fn different_body_with_same_key_is_rejected() {
        let root = tempfile::tempdir().expect("root");
        let executor = FakeExecutor::default();
        let service = GitHubService::enterprise(
            "acme/project",
            "github.com",
            Some(root.path().to_path_buf()),
            executor,
        );
        let coordinator = GitHubOperationCoordinator::new(
            service,
            NoDirectGitHubBackend,
            FileGitHubOperationLedger::at(root.path()).expect("ledger"),
        );
        let mut first = document(Some("same-key"));
        first.operation = GitHubTypedOperation::Issues(IssuesOperation::Create {
            title: "first".into(),
            body: "body".into(),
            labels: vec![],
            assignees: vec![],
            milestone: None,
        });
        coordinator.execute(&first).expect("first");
        let mut second = first.clone();
        second.operation = GitHubTypedOperation::Issues(IssuesOperation::Create {
            title: "different".into(),
            body: "body".into(),
            labels: vec![],
            assignees: vec![],
            milestone: None,
        });
        assert!(coordinator.execute(&second).is_err());
    }
}
