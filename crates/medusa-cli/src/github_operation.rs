use std::{
    collections::BTreeSet,
    env,
    fs::{self, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
};

use clap::Parser;
use medusa_capabilities::{
    Capability, CapabilityAuthorizer, CapabilityGrant, CapabilityPermission, CapabilityRegistry,
    github_descriptor,
};
use medusa_github::{
    GitHubOperationReceipt, GitHubOperationRequest, GitHubOperationRisk, GitHubService,
    SystemExecutor,
};
use serde::Serialize;

#[derive(Debug, Parser)]
#[command(name = "medusa-github-operation")]
#[command(
    about = "Execute one repository-confined GitHub operation through an interchangeable backend"
)]
struct Cli {
    /// JSON operation document path, or `-` to read from standard input.
    #[arg(long, value_name = "PATH")]
    request: PathBuf,

    /// Repository directory used for capability discovery and local command context.
    #[arg(long, value_name = "PATH")]
    repository_directory: Option<PathBuf>,

    /// Explicitly approve an ordinary GitHub mutation.
    #[arg(long)]
    approve: bool,

    /// Separately approve administrative, secret, or destructive GitHub operations.
    #[arg(long)]
    approve_high_risk: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct OperationOutput {
    receipt: GitHubOperationReceipt,
    audit_path: PathBuf,
    audit_error: Option<String>,
    authorization_events: Vec<medusa_capabilities::CapabilityAuditEvent>,
}

fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    let request = read_request(&cli.request)?;
    request.validate()?;

    let repository_directory = cli
        .repository_directory
        .clone()
        .unwrap_or(env::current_dir()?);
    if !repository_directory.is_dir() {
        return Err(medusa_core::MedusaError::new(
            medusa_core::ErrorCode::InvalidInput,
            medusa_core::ErrorCategory::Validation,
            "--repository-directory must identify an existing directory",
        )
        .into());
    }

    let registry = CapabilityRegistry::discover(&repository_directory)?;
    let grants = vec![CapabilityGrant {
        capability: Capability::GitHub,
        permissions: BTreeSet::from([
            CapabilityPermission::Read,
            CapabilityPermission::Write,
            CapabilityPermission::RepositoryMutation,
        ]),
    }];
    let mut authorizer = CapabilityAuthorizer::new(registry, grants);
    authorizer.require(&github_descriptor(), CapabilityPermission::Read, false)?;
    authorize_operation(&mut authorizer, request.risk(), &cli)?;

    eprintln!(
        "GitHub operation preview:\n{}",
        serde_json::to_string_pretty(&request.redacted_preview())?
    );

    let audit_path = prepare_audit()?;
    let service = GitHubService::enterprise(
        request.repository.clone(),
        request.hostname.clone(),
        Some(repository_directory),
        SystemExecutor,
    );
    let receipt = service.execute_operation(&request)?;
    let authorization_events = authorizer.events().to_vec();
    match append_audit(&audit_path, &receipt, &authorization_events) {
        Ok(()) => {
            println!(
                "{}",
                serde_json::to_string_pretty(&OperationOutput {
                    receipt,
                    audit_path,
                    audit_error: None,
                    authorization_events,
                })?
            );
            Ok(())
        }
        Err(error) => {
            println!(
                "{}",
                serde_json::to_string_pretty(&OperationOutput {
                    receipt: receipt.clone(),
                    audit_path: audit_path.clone(),
                    audit_error: Some(error.to_string()),
                    authorization_events,
                })?
            );
            Err(medusa_core::MedusaError::new(
                medusa_core::ErrorCode::PersistenceFailed,
                medusa_core::ErrorCategory::Persistence,
                format!(
                    "GitHub operation {} completed, but its audit receipt could not be persisted at {}: {error}",
                    receipt.operation_id,
                    audit_path.display()
                ),
            )
            .with_retryable(request.risk().requires_approval())
            .into())
        }
    }
}

fn authorize_operation(
    authorizer: &mut CapabilityAuthorizer,
    risk: GitHubOperationRisk,
    cli: &Cli,
) -> medusa_core::MedusaResult<()> {
    if risk.requires_approval() {
        authorizer.require(
            &github_descriptor(),
            CapabilityPermission::Write,
            cli.approve,
        )?;
    }
    if risk.high_risk() {
        authorizer.require(
            &github_descriptor(),
            CapabilityPermission::RepositoryMutation,
            cli.approve_high_risk,
        )?;
    }
    Ok(())
}

fn read_request(path: &Path) -> Result<GitHubOperationRequest, Box<dyn std::error::Error>> {
    let mut encoded = String::new();
    if path == Path::new("-") {
        io::stdin().read_to_string(&mut encoded)?;
    } else {
        encoded = fs::read_to_string(path)?;
    }
    let request = serde_json::from_str(&encoded)?;
    Ok(request)
}

fn prepare_audit() -> Result<PathBuf, Box<dyn std::error::Error>> {
    let root = env::var_os("MEDUSA_HOME")
        .map(PathBuf::from)
        .unwrap_or(env::current_dir()?.join(".medusa"));
    prepare_audit_at(&root)
}

fn prepare_audit_at(root: &Path) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let path = root.join("audit").join("github-operations.jsonl");
    let parent = path.parent().ok_or_else(|| {
        medusa_core::MedusaError::new(
            medusa_core::ErrorCode::PersistenceFailed,
            medusa_core::ErrorCategory::Persistence,
            "invalid GitHub operation audit path",
        )
    })?;
    fs::create_dir_all(parent)?;
    let file = OpenOptions::new().create(true).append(true).open(&path)?;
    file.sync_all()?;
    Ok(path)
}

fn append_audit(
    path: &Path,
    receipt: &GitHubOperationReceipt,
    authorization_events: &[medusa_capabilities::CapabilityAuditEvent],
) -> Result<(), Box<dyn std::error::Error>> {
    let record = serde_json::json!({
        "receipt": receipt,
        "authorizationEvents": authorization_events,
    });
    let mut encoded = serde_json::to_vec(&record)?;
    encoded.push(b'\n');
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    file.write_all(&encoded)?;
    file.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use medusa_capabilities::CapabilityState;
    use medusa_github::{GitHubBackendKind, GitHubHttpMethod, GitHubResource};
    use serde_json::Value;

    fn request(risk: GitHubOperationRisk) -> GitHubOperationRequest {
        let (resource, method, endpoint) = match risk {
            GitHubOperationRisk::ReadOnly => (
                GitHubResource::Repository,
                GitHubHttpMethod::Get,
                String::new(),
            ),
            GitHubOperationRisk::Mutation => (
                GitHubResource::Issues,
                GitHubHttpMethod::Post,
                "issues".into(),
            ),
            GitHubOperationRisk::Administration => (
                GitHubResource::Collaborators,
                GitHubHttpMethod::Put,
                "collaborators/octocat".into(),
            ),
            GitHubOperationRisk::Secret => (
                GitHubResource::Secrets,
                GitHubHttpMethod::Put,
                "actions/secrets/DEPLOY_TOKEN".into(),
            ),
            GitHubOperationRisk::Destructive => (
                GitHubResource::Issues,
                GitHubHttpMethod::Delete,
                "issues/7".into(),
            ),
        };
        GitHubOperationRequest {
            repository: "acme/project".into(),
            hostname: "github.com".into(),
            resource,
            action: "test".into(),
            method,
            endpoint,
            query: Default::default(),
            body: if matches!(method, GitHubHttpMethod::Get | GitHubHttpMethod::Delete) {
                None
            } else {
                Some(serde_json::json!({"title":"test"}))
            },
            backend: GitHubBackendKind::RestApi,
            paginate: false,
            max_response_bytes: 1024,
        }
    }

    fn authorizer() -> CapabilityAuthorizer {
        let registry = CapabilityRegistry {
            repository: PathBuf::from("."),
            capabilities: BTreeMap::from([(
                Capability::GitHub,
                CapabilityState {
                    available: true,
                    detail: "test GitHub backend".into(),
                },
            )]),
            entries: BTreeMap::new(),
        };
        CapabilityAuthorizer::new(
            registry,
            vec![CapabilityGrant {
                capability: Capability::GitHub,
                permissions: BTreeSet::from([
                    CapabilityPermission::Read,
                    CapabilityPermission::Write,
                    CapabilityPermission::RepositoryMutation,
                ]),
            }],
        )
    }

    #[test]
    fn cli_requires_two_distinct_approvals_for_high_risk_operations() {
        let cli = Cli::try_parse_from([
            "medusa-github-operation",
            "--request",
            "operation.json",
            "--approve",
        ])
        .expect("parse");
        let mut authorizer = authorizer();
        assert!(
            authorize_operation(
                &mut authorizer,
                request(GitHubOperationRisk::Administration).risk(),
                &cli,
            )
            .is_err()
        );
    }

    #[test]
    fn read_only_operation_requires_no_approval_flags() {
        let cli = Cli::try_parse_from(["medusa-github-operation", "--request", "operation.json"])
            .expect("parse");
        let mut authorizer = authorizer();
        authorize_operation(
            &mut authorizer,
            request(GitHubOperationRisk::ReadOnly).risk(),
            &cli,
        )
        .expect("read");
    }

    #[test]
    fn audit_is_one_line_and_contains_no_secret_request_body() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = prepare_audit_at(directory.path()).expect("audit");
        let operation = request(GitHubOperationRisk::Secret);
        let risk = operation.risk();
        let receipt = GitHubOperationReceipt {
            operation_id: "github-1".into(),
            repository: operation.repository,
            hostname: operation.hostname,
            resource: operation.resource,
            action: operation.action,
            method: operation.method,
            endpoint: operation.endpoint,
            risk,
            backend: operation.backend,
            transport: "gh_api".into(),
            mutated: true,
            resource_identity: Some("DEPLOY_TOKEN".into()),
            resource_url: None,
            canonical: Value::Object(Default::default()),
            payload: serde_json::json!({"name":"DEPLOY_TOKEN","value":"[REDACTED]"}),
            truncated: false,
        };
        append_audit(&path, &receipt, &[]).expect("append");
        let content = fs::read_to_string(path).expect("read");
        assert_eq!(content.lines().count(), 1);
        assert!(!content.contains("super-secret"));
    }
}
