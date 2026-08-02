use std::{
    collections::BTreeSet,
    env,
    fs::{self, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
};

use clap::{Args, Parser, Subcommand};
use medusa_capabilities::{
    Capability, CapabilityAuthorizer, CapabilityGrant, CapabilityPermission, CapabilityRegistry,
    github_descriptor,
};
use medusa_github::{
    DirectGitHubOperationExecutor, DirectGitHubBackend, FileGitHubOperationLedger,
    GitHubCapabilityReport, GitHubExecutionBackend, GitHubOAuthClient, GitHubOAuthConfig,
    GitHubOAuthStatus, GitHubOperationCoordinator, GitHubOperationDocument,
    GitHubOperationReceiptV2, GitHubOperationRequest, GitHubOperationRisk, GitHubService,
    KeyringGitHubCredentialStore, NoDirectGitHubBackend, ReqwestGitHubApiTransport,
    ReqwestOAuthTransport, SystemExecutor, convert_legacy_github_operation,
};
use serde::Serialize;

#[derive(Debug, Parser)]
#[command(name = "medusa-github-operation")]
#[command(about = "Typed, approval-gated GitHub management with direct OAuth support")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,

    /// Backwards-compatible execute form. Prefer `execute --request PATH`.
    #[command(flatten)]
    legacy_execute: ExecuteArgs,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Execute one typed repository-confined operation.
    Execute(ExecuteArgs),
    /// Manage the direct GitHub App OAuth credential.
    Auth(AuthArgs),
    /// Inspect authentication, repository permissions, rate limits, and supported operations.
    Capabilities(CapabilitiesArgs),
}

#[derive(Clone, Debug, Args)]
struct ExecuteArgs {
    /// Versioned JSON operation document path, or `-` for standard input.
    #[arg(long, value_name = "PATH")]
    request: Option<PathBuf>,

    /// Repository directory used for capability discovery, state, and confined artifacts.
    #[arg(long, value_name = "PATH")]
    repository_directory: Option<PathBuf>,

    /// Public GitHub App client ID. May also be set as MEDUSA_GITHUB_CLIENT_ID.
    #[arg(long, value_name = "CLIENT_ID")]
    client_id: Option<String>,

    /// Explicitly approve an ordinary GitHub mutation.
    #[arg(long)]
    approve: bool,

    /// Separately approve administrative, secret, or destructive operations.
    #[arg(long)]
    approve_high_risk: bool,
}

#[derive(Debug, Args)]
struct CapabilitiesArgs {
    /// Versioned JSON operation document used to evaluate exact permission requirements.
    #[arg(long, value_name = "PATH")]
    request: PathBuf,

    /// Repository directory used for state and capability discovery.
    #[arg(long, value_name = "PATH")]
    repository_directory: Option<PathBuf>,

    /// Public GitHub App client ID. May also be set as MEDUSA_GITHUB_CLIENT_ID.
    #[arg(long, value_name = "CLIENT_ID")]
    client_id: Option<String>,
}

#[derive(Debug, Args)]
struct AuthArgs {
    #[command(subcommand)]
    command: AuthCommand,

    /// Public GitHub App client ID. May also be set as MEDUSA_GITHUB_CLIENT_ID.
    #[arg(long, value_name = "CLIENT_ID")]
    client_id: Option<String>,

    /// GitHub hostname.
    #[arg(long, default_value = "github.com")]
    hostname: String,

    /// Optional GitHub OAuth base URL for GHE.com or GitHub Enterprise Server.
    #[arg(long, value_name = "URL")]
    oauth_base_url: Option<String>,

    /// Optional GitHub API base URL for GHE.com or GitHub Enterprise Server.
    #[arg(long, value_name = "URL")]
    api_base_url: Option<String>,

    /// Pinned GitHub REST API version.
    #[arg(long, default_value = "2022-11-28")]
    api_version: String,
}

#[derive(Debug, Subcommand)]
enum AuthCommand {
    /// Start the GitHub App device flow and wait for authorization.
    Login,
    /// Report credential presence, identity, expiry, and refresh readiness.
    Status,
    /// Rotate an expiring GitHub App user token and refresh token.
    Refresh,
    /// Delete the GitHub credential from the operating-system credential store.
    Logout,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct OperationOutput {
    receipt: GitHubOperationReceiptV2,
    ledger_path: PathBuf,
    authorization_audit_path: PathBuf,
    authorization_events: Vec<medusa_capabilities::CapabilityAuditEvent>,
    compatibility_warning: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct DeviceLoginOutput {
    verification_uri: String,
    user_code: String,
    expires_at: i64,
    interval_seconds: u64,
    status: GitHubOAuthStatus,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct LogoutOutput {
    hostname: String,
    removed: bool,
    credential_backend: &'static str,
}

fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    match cli.command {
        Some(Command::Execute(args)) => execute(args),
        Some(Command::Auth(args)) => auth(args),
        Some(Command::Capabilities(args)) => capabilities(args),
        None => execute(cli.legacy_execute),
    }
}

fn execute(args: ExecuteArgs) -> Result<(), Box<dyn std::error::Error>> {
    let request_path = args.request.as_deref().ok_or_else(|| {
        medusa_core::MedusaError::new(
            medusa_core::ErrorCode::InvalidInput,
            medusa_core::ErrorCategory::Validation,
            "--request is required; use `execute --request PATH`",
        )
    })?;
    let (document, compatibility_warning) = read_document(request_path)?;
    document.validate()?;
    let repository_directory = repository_directory(args.repository_directory)?;

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
    authorize_operation(&mut authorizer, document.risk()?, &args)?;

    eprintln!(
        "GitHub typed operation preview:\n{}",
        serde_json::to_string_pretty(&document.redacted_preview()?)?
    );
    if let Some(warning) = compatibility_warning.as_deref() {
        eprintln!("warning: {warning}");
    }

    let state_root = state_root(&repository_directory)?;
    let authorization_audit_path = prepare_authorization_audit(&state_root)?;
    let authorization_events = authorizer.events().to_vec();
    append_authorization_audit(
        &authorization_audit_path,
        &document,
        &authorization_events,
    )?;
    let ledger = FileGitHubOperationLedger::at(&state_root)?;
    let ledger_path = ledger.path().to_path_buf();
    let service = GitHubService::enterprise(
        document.repository.clone(),
        document.hostname.clone(),
        Some(repository_directory.clone()),
        SystemExecutor,
    );

    let receipt = if matches!(document.backend, GitHubExecutionBackend::DirectOauth) {
        let oauth = oauth_client(oauth_config_for_document(&document, args.client_id)?)?;
        let direct = DirectGitHubBackend::new(
            oauth,
            ReqwestGitHubApiTransport::new()?,
            repository_directory,
        )?;
        execute_with_backend(service, direct, ledger, &document)?
    } else {
        execute_with_backend(service, NoDirectGitHubBackend, ledger, &document)?
    };

    println!(
        "{}",
        serde_json::to_string_pretty(&OperationOutput {
            receipt,
            ledger_path,
            authorization_audit_path,
            authorization_events,
            compatibility_warning,
        })?
    );
    Ok(())
}

fn execute_with_backend<D: DirectGitHubOperationExecutor>(
    service: GitHubService<SystemExecutor>,
    direct: D,
    ledger: FileGitHubOperationLedger,
    document: &GitHubOperationDocument,
) -> medusa_core::MedusaResult<GitHubOperationReceiptV2> {
    GitHubOperationCoordinator::new(service, direct, ledger).execute(document)
}

fn capabilities(args: CapabilitiesArgs) -> Result<(), Box<dyn std::error::Error>> {
    let (document, compatibility_warning) = read_document(&args.request)?;
    document.validate()?;
    if let Some(warning) = compatibility_warning {
        eprintln!("warning: {warning}");
    }
    let repository_directory = repository_directory(args.repository_directory)?;
    let state_root = state_root(&repository_directory)?;
    let ledger = FileGitHubOperationLedger::at(&state_root)?;
    let service = GitHubService::enterprise(
        document.repository.clone(),
        document.hostname.clone(),
        Some(repository_directory.clone()),
        SystemExecutor,
    );
    let report = if matches!(document.backend, GitHubExecutionBackend::DirectOauth) {
        let oauth = oauth_client(oauth_config_for_document(&document, args.client_id)?)?;
        let direct = DirectGitHubBackend::new(
            oauth,
            ReqwestGitHubApiTransport::new()?,
            repository_directory,
        )?;
        capabilities_with_backend(service, direct, ledger, &document)?
    } else {
        capabilities_with_backend(service, NoDirectGitHubBackend, ledger, &document)?
    };
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}

fn capabilities_with_backend<D: DirectGitHubOperationExecutor>(
    service: GitHubService<SystemExecutor>,
    direct: D,
    ledger: FileGitHubOperationLedger,
    document: &GitHubOperationDocument,
) -> medusa_core::MedusaResult<GitHubCapabilityReport> {
    GitHubOperationCoordinator::new(service, direct, ledger).capabilities(document)
}

fn auth(args: AuthArgs) -> Result<(), Box<dyn std::error::Error>> {
    let config = GitHubOAuthConfig {
        client_id: client_id(args.client_id)?,
        hostname: args.hostname,
        oauth_base_url: args.oauth_base_url,
        api_base_url: args.api_base_url,
        api_version: args.api_version,
    };
    let client = oauth_client(config.clone())?;
    match args.command {
        AuthCommand::Login => {
            let authorization = client.start_device_flow()?;
            eprintln!(
                "Open {} and enter code {}. This code is not a credential.",
                authorization.verification_uri(),
                authorization.user_code()
            );
            let verification_uri = authorization.verification_uri().to_owned();
            let user_code = authorization.user_code().to_owned();
            let expires_at = authorization.expires_at();
            let interval_seconds = authorization.interval_seconds();
            let status = client.complete_device_flow(&authorization)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&DeviceLoginOutput {
                    verification_uri,
                    user_code,
                    expires_at,
                    interval_seconds,
                    status,
                })?
            );
        }
        AuthCommand::Status => {
            println!("{}", serde_json::to_string_pretty(&client.status()?)?);
        }
        AuthCommand::Refresh => {
            println!("{}", serde_json::to_string_pretty(&client.refresh()?)?);
        }
        AuthCommand::Logout => {
            println!(
                "{}",
                serde_json::to_string_pretty(&LogoutOutput {
                    hostname: config.hostname,
                    removed: client.logout()?,
                    credential_backend: "operating-system credential store",
                })?
            );
        }
    }
    Ok(())
}

fn oauth_client(
    config: GitHubOAuthConfig,
) -> medusa_core::MedusaResult<
    GitHubOAuthClient<KeyringGitHubCredentialStore, ReqwestOAuthTransport>,
> {
    GitHubOAuthClient::new(
        config,
        KeyringGitHubCredentialStore::default(),
        ReqwestOAuthTransport::new()?,
    )
}

fn oauth_config_for_document(
    document: &GitHubOperationDocument,
    explicit_client_id: Option<String>,
) -> Result<GitHubOAuthConfig, Box<dyn std::error::Error>> {
    Ok(GitHubOAuthConfig {
        client_id: client_id(explicit_client_id)?,
        hostname: document.hostname.clone(),
        oauth_base_url: document.oauth_base_url.clone(),
        api_base_url: document.api_base_url.clone(),
        api_version: document.api_version.clone(),
    })
}

fn client_id(explicit: Option<String>) -> Result<String, Box<dyn std::error::Error>> {
    explicit
        .or_else(|| env::var("MEDUSA_GITHUB_CLIENT_ID").ok())
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
            medusa_core::MedusaError::new(
                medusa_core::ErrorCode::InvalidConfiguration,
                medusa_core::ErrorCategory::Environment,
                "GitHub App public client ID is required through --client-id or MEDUSA_GITHUB_CLIENT_ID",
            )
            .into()
        })
}

fn authorize_operation(
    authorizer: &mut CapabilityAuthorizer,
    risk: GitHubOperationRisk,
    args: &ExecuteArgs,
) -> medusa_core::MedusaResult<()> {
    if risk.requires_approval() {
        authorizer.require(
            &github_descriptor(),
            CapabilityPermission::Write,
            args.approve,
        )?;
    }
    if risk.high_risk() {
        authorizer.require(
            &github_descriptor(),
            CapabilityPermission::RepositoryMutation,
            args.approve_high_risk,
        )?;
    }
    Ok(())
}

fn read_document(
    path: &Path,
) -> Result<(GitHubOperationDocument, Option<String>), Box<dyn std::error::Error>> {
    let mut encoded = String::new();
    if path == Path::new("-") {
        io::stdin().read_to_string(&mut encoded)?;
    } else {
        encoded = fs::read_to_string(path)?;
    }
    let value: serde_json::Value = serde_json::from_str(&encoded)?;
    if value.get("schemaVersion").is_some() || value.get("schema_version").is_some() {
        return Ok((serde_json::from_value(value)?, None));
    }
    let legacy: GitHubOperationRequest = serde_json::from_value(value)?;
    let conversion = convert_legacy_github_operation(legacy)?;
    Ok((conversion.document, Some(conversion.warning)))
}

fn repository_directory(explicit: Option<PathBuf>) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let path = explicit.unwrap_or(env::current_dir()?);
    let canonical = path.canonicalize()?;
    if !canonical.is_dir() {
        return Err(medusa_core::MedusaError::new(
            medusa_core::ErrorCode::InvalidInput,
            medusa_core::ErrorCategory::Validation,
            "--repository-directory must identify an existing directory",
        )
        .into());
    }
    Ok(canonical)
}

fn state_root(repository_directory: &Path) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let root = env::var_os("MEDUSA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| repository_directory.join(".medusa"));
    fs::create_dir_all(&root)?;
    Ok(root)
}

fn prepare_authorization_audit(root: &Path) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let path = root.join("audit").join("github-operation-authorizations.jsonl");
    let parent = path.parent().ok_or_else(|| {
        medusa_core::MedusaError::new(
            medusa_core::ErrorCode::PersistenceFailed,
            medusa_core::ErrorCategory::Persistence,
            "invalid GitHub authorization audit path",
        )
    })?;
    fs::create_dir_all(parent)?;
    let file = OpenOptions::new().create(true).append(true).open(&path)?;
    file.sync_all()?;
    Ok(path)
}

fn append_authorization_audit(
    path: &Path,
    document: &GitHubOperationDocument,
    authorization_events: &[medusa_capabilities::CapabilityAuditEvent],
) -> Result<(), Box<dyn std::error::Error>> {
    let record = serde_json::json!({
        "requestDigest": document.request_digest()?,
        "idempotencyKeyHash": document.idempotency_key_hash(),
        "repository": document.repository,
        "hostname": document.hostname,
        "operation": document.prepare()?.operation_kind,
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
    use medusa_github::{
        GITHUB_OPERATION_SCHEMA_VERSION, GitHubBackendKind, GitHubHttpMethod, GitHubResource,
        GitHubTypedOperation, RepositoryOperation,
    };

    fn typed_document() -> GitHubOperationDocument {
        GitHubOperationDocument {
            schema_version: GITHUB_OPERATION_SCHEMA_VERSION,
            repository: "acme/project".into(),
            hostname: "github.com".into(),
            api_base_url: None,
            oauth_base_url: None,
            api_version: "2022-11-28".into(),
            backend: GitHubExecutionBackend::GhApi,
            idempotency_key: None,
            max_response_bytes: 1024,
            operation: GitHubTypedOperation::Repository(RepositoryOperation::Get),
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
    fn high_risk_operation_requires_both_approvals() {
        let args = ExecuteArgs {
            request: Some(PathBuf::from("request.json")),
            repository_directory: None,
            client_id: None,
            approve: true,
            approve_high_risk: false,
        };
        let mut authorizer = authorizer();
        assert!(
            authorize_operation(
                &mut authorizer,
                GitHubOperationRisk::Administration,
                &args,
            )
            .is_err()
        );
    }

    #[test]
    fn typed_document_round_trips_without_warning() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("request.json");
        fs::write(&path, serde_json::to_vec(&typed_document()).expect("encode")).expect("write");
        let (parsed, warning) = read_document(&path).expect("read");
        assert_eq!(parsed, typed_document());
        assert!(warning.is_none());
    }

    #[test]
    fn known_legacy_document_is_converted_with_warning() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("legacy.json");
        let legacy = GitHubOperationRequest {
            repository: "acme/project".into(),
            hostname: "github.com".into(),
            resource: GitHubResource::Repository,
            action: "get".into(),
            method: GitHubHttpMethod::Get,
            endpoint: String::new(),
            query: BTreeMap::new(),
            body: None,
            backend: GitHubBackendKind::RestApi,
            paginate: false,
            max_response_bytes: 1024,
        };
        fs::write(&path, serde_json::to_vec(&legacy).expect("encode")).expect("write");
        let (_, warning) = read_document(&path).expect("read");
        assert!(warning.expect("warning").contains("deprecated"));
    }

    #[test]
    fn client_secret_is_not_a_supported_argument() {
        assert!(Cli::try_parse_from([
            "medusa-github-operation",
            "auth",
            "--client-secret",
            "secret",
            "status",
        ])
        .is_err());
    }
}
