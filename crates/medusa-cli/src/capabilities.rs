use std::{
    collections::BTreeSet,
    env,
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

use clap::{Args, Parser, Subcommand, ValueEnum};
use medusa_capabilities::{
    Capability, CapabilityAuthorizer, CapabilityGrant, CapabilityPermission, CapabilityRegistry,
    github_descriptor,
};
use medusa_github::{
    GitHubService, RepositoryBootstrap, RepositoryCreateRequest, RepositoryCreationReceipt,
    RepositoryVisibility, SystemExecutor,
};
use serde::Serialize;

#[derive(Debug, Parser)]
#[command(name = "medusa-capabilities")]
#[command(about = "Inspect and invoke Medusa's explicitly authorized capabilities")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,

    /// Repository path used for capability discovery in diagnostics mode.
    #[arg(value_name = "REPOSITORY_PATH", default_value = ".")]
    repository_path: PathBuf,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Create and optionally bootstrap a GitHub repository after explicit approval.
    CreateRepository(CreateRepositoryArgs),
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum VisibilityArgument {
    Public,
    Private,
    Internal,
}

impl From<VisibilityArgument> for RepositoryVisibility {
    fn from(value: VisibilityArgument) -> Self {
        match value {
            VisibilityArgument::Public => Self::Public,
            VisibilityArgument::Private => Self::Private,
            VisibilityArgument::Internal => Self::Internal,
        }
    }
}

#[derive(Debug, Args)]
struct CreateRepositoryArgs {
    #[arg(long)]
    owner: String,
    #[arg(long)]
    name: String,
    #[arg(long, value_enum, default_value = "private")]
    visibility: VisibilityArgument,
    #[arg(long)]
    description: Option<String>,
    #[arg(long)]
    homepage: Option<String>,
    #[arg(long, default_value = "main")]
    default_branch: String,
    #[arg(long)]
    add_readme: bool,
    #[arg(long)]
    gitignore: Option<String>,
    #[arg(long)]
    license: Option<String>,
    #[arg(long)]
    disable_issues: bool,
    #[arg(long)]
    disable_wiki: bool,
    #[arg(long)]
    template: Option<String>,
    #[arg(long)]
    include_all_template_branches: bool,
    #[arg(long)]
    reuse_existing: bool,
    #[arg(long)]
    source: Option<PathBuf>,
    #[arg(long)]
    initialize_git: bool,
    #[arg(long)]
    initial_commit_message: Option<String>,
    #[arg(long)]
    no_push: bool,
    #[arg(long, default_value = "github.com")]
    hostname: String,
    /// Confirms creation of the external persistent GitHub resource.
    #[arg(long)]
    approve: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RepositoryCreationOutput {
    receipt: RepositoryCreationReceipt,
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
    match cli.command {
        Some(Command::CreateRepository(arguments)) => create_repository(arguments),
        None => diagnostics(cli.repository_path),
    }
}

fn diagnostics(repository_path: PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    let registry = CapabilityRegistry::discover(&repository_path)?;
    println!(
        "{}",
        serde_json::to_string_pretty(&registry.protocol_report())?
    );
    Ok(())
}

fn create_repository(arguments: CreateRepositoryArgs) -> Result<(), Box<dyn std::error::Error>> {
    let CreateRepositoryArgs {
        owner,
        name,
        visibility,
        description,
        homepage,
        default_branch,
        add_readme,
        gitignore,
        license,
        disable_issues,
        disable_wiki,
        template,
        include_all_template_branches,
        reuse_existing,
        source,
        initialize_git,
        initial_commit_message,
        no_push,
        hostname,
        approve,
    } = arguments;

    if source.is_none() && (initialize_git || initial_commit_message.is_some() || no_push) {
        return Err(medusa_core::MedusaError::new(
            medusa_core::ErrorCode::InvalidInput,
            medusa_core::ErrorCategory::Validation,
            "--initialize-git, --initial-commit-message, and --no-push require --source",
        )
        .into());
    }

    let discovery_path = source
        .as_deref()
        .filter(|path| path.is_dir())
        .map_or_else(|| PathBuf::from("."), PathBuf::from);
    let registry = CapabilityRegistry::discover(&discovery_path)?;
    let grants = vec![CapabilityGrant {
        capability: Capability::GitHub,
        permissions: BTreeSet::from([
            CapabilityPermission::Read,
            CapabilityPermission::RepositoryMutation,
        ]),
    }];
    let mut authorizer = CapabilityAuthorizer::new(registry, grants);
    authorizer.require(&github_descriptor(), CapabilityPermission::Read, false)?;

    let bootstrap = source.map(|path| RepositoryBootstrap {
        path,
        initialize_git,
        initial_commit_message,
        push: !no_push,
    });
    let request = RepositoryCreateRequest {
        owner,
        name,
        visibility: visibility.into(),
        description,
        homepage,
        default_branch,
        add_readme,
        gitignore_template: gitignore,
        license_template: license,
        issues_enabled: !disable_issues,
        wiki_enabled: !disable_wiki,
        template_repository: template,
        include_all_template_branches,
        reuse_existing,
        bootstrap,
    };
    request.validate()?;

    let preview = serde_json::to_string_pretty(&request)?;
    eprintln!("Repository creation preview:\n{preview}");
    authorizer.require(
        &github_descriptor(),
        CapabilityPermission::RepositoryMutation,
        approve,
    )?;

    let audit_path = prepare_repository_creation_audit()?;
    let full_name = format!("{}/{}", request.owner.trim(), request.name.trim());
    let service = GitHubService::enterprise(full_name, hostname, None, SystemExecutor);
    let receipt = service.create_repository(&request)?;
    let authorization_events = authorizer.events().to_vec();
    match append_repository_creation_audit(&audit_path, &receipt, &authorization_events) {
        Ok(()) => {
            let output = RepositoryCreationOutput {
                receipt,
                audit_path,
                audit_error: None,
                authorization_events,
            };
            println!("{}", serde_json::to_string_pretty(&output)?);
            Ok(())
        }
        Err(error) => {
            let output = RepositoryCreationOutput {
                receipt: receipt.clone(),
                audit_path: audit_path.clone(),
                audit_error: Some(error.to_string()),
                authorization_events,
            };
            println!("{}", serde_json::to_string_pretty(&output)?);
            Err(medusa_core::MedusaError::new(
                medusa_core::ErrorCode::PersistenceFailed,
                medusa_core::ErrorCategory::Persistence,
                format!(
                    "repository {} was created, but its audit receipt could not be persisted at {}: {error}; retain the printed receipt and retry with --reuse-existing after repairing the audit destination",
                    receipt.repository,
                    audit_path.display()
                ),
            )
            .with_retryable(true)
            .into())
        }
    }
}

fn prepare_repository_creation_audit() -> Result<PathBuf, Box<dyn std::error::Error>> {
    let root = if let Some(value) = env::var_os("MEDUSA_HOME") {
        PathBuf::from(value)
    } else {
        env::current_dir()?.join(".medusa")
    };
    prepare_repository_creation_audit_at(&root)
}

fn prepare_repository_creation_audit_at(
    root: &Path,
) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let path = root.join("audit").join("github-repository-creation.jsonl");
    let parent = path.parent().ok_or_else(|| {
        medusa_core::MedusaError::new(
            medusa_core::ErrorCode::PersistenceFailed,
            medusa_core::ErrorCategory::Persistence,
            "invalid GitHub repository creation audit path",
        )
    })?;
    fs::create_dir_all(parent)?;
    let file = OpenOptions::new().create(true).append(true).open(&path)?;
    file.sync_all()?;
    Ok(path)
}

fn append_repository_creation_audit(
    path: &Path,
    receipt: &RepositoryCreationReceipt,
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
    use super::*;

    #[test]
    fn create_repository_requires_explicit_approval_flag() {
        let cli = Cli::try_parse_from([
            "medusa-capabilities",
            "create-repository",
            "--owner",
            "acme",
            "--name",
            "project",
        ])
        .expect("parse");
        let Some(Command::CreateRepository(arguments)) = cli.command else {
            panic!("create command");
        };
        assert!(!arguments.approve);
        assert!(matches!(arguments.visibility, VisibilityArgument::Private));
    }

    #[test]
    fn diagnostics_mode_remains_backwards_compatible() {
        let cli = Cli::try_parse_from(["medusa-capabilities", "checkout"]).expect("parse");
        assert!(cli.command.is_none());
        assert_eq!(cli.repository_path, PathBuf::from("checkout"));
    }

    #[test]
    fn preflights_and_persists_one_line_redacted_repository_creation_evidence() {
        let directory = tempfile::tempdir().expect("tempdir");
        let receipt = RepositoryCreationReceipt {
            repository: "acme/project".into(),
            web_url: "https://github.example/acme/project".into(),
            clone_url: "https://github.example/acme/project.git".into(),
            visibility: RepositoryVisibility::Private,
            default_branch: "main".into(),
            created: true,
            local_path: Some(directory.path().join("project")),
            initial_commit: Some("abc123".into()),
        };
        let path = prepare_repository_creation_audit_at(directory.path()).expect("preflight");
        assert!(path.is_file());
        assert!(!path.starts_with(receipt.local_path.as_ref().expect("local path")));
        append_repository_creation_audit(&path, &receipt, &[]).expect("persist");
        let content = fs::read_to_string(path).expect("read");
        assert_eq!(content.lines().count(), 1);
        assert!(!content.contains("token"));
    }
}
