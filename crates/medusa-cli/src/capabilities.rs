use std::{collections::BTreeSet, env, path::PathBuf};

use medusa_capabilities::{
    Capability, CapabilityGrant, CapabilityPermission, CapabilityRegistry,
    ExplicitCapabilityRuntime,
};
use medusa_github::GitHubService;

fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let repository_path = env::args().nth(1).map_or_else(|| PathBuf::from("."), PathBuf::from);
    let repository_name = env::var("MEDUSA_GITHUB_REPOSITORY").unwrap_or_else(|_| "local/repository".into());
    let registry = CapabilityRegistry::discover(&repository_path)?;
    let grants = vec![
        CapabilityGrant {
            capability: Capability::GitHub,
            permissions: BTreeSet::from([CapabilityPermission::Read]),
        },
        CapabilityGrant {
            capability: Capability::SelfImprovement,
            permissions: BTreeSet::from([CapabilityPermission::Read]),
        },
    ];
    let runtime = ExplicitCapabilityRuntime::new(
        registry,
        grants,
        GitHubService::new(repository_name),
    );
    let diagnostics = runtime.diagnostics();
    println!("{}", serde_json::to_string_pretty(&diagnostics)?);
    Ok(())
}
