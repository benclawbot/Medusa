use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
    thread,
};

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use medusa_evidence::{VerificationCheck, VerificationCheckKind, VerificationPlan};
use sha2::{Digest, Sha256};

use crate::{
    verification::{ExecutedVerificationCommand, execute_verification_command},
    verification_dag::{
        VerificationAuthority, VerificationDag, VerificationInputKey, VerificationNode,
        VerificationNodeState,
    },
};

pub(crate) fn dag_for_plan(
    repo: &Path,
    commit: &str,
    plan: &VerificationPlan,
) -> MedusaResult<VerificationDag> {
    let changed_paths = plan
        .components
        .iter()
        .flat_map(|component| component.all_paths())
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();
    let environment_fingerprint = fingerprint(&format!(
        "{}:{}:{}",
        std::env::consts::OS,
        std::env::consts::ARCH,
        verifier_build_fingerprint()?
    ));
    let toolchain_fingerprint = toolchain_fingerprint(repo);
    let mut dag = VerificationDag::default();
    let mut remaining = plan
        .checks
        .iter()
        .map(|check| (check.id.clone(), check))
        .collect::<BTreeMap<_, _>>();

    while !remaining.is_empty() {
        let ready_ids = remaining
            .iter()
            .filter_map(|(id, check)| {
                dependencies(plan, check)
                    .iter()
                    .all(|dependency| dag.node(dependency).is_some())
                    .then_some(id.clone())
            })
            .collect::<Vec<_>>();
        if ready_ids.is_empty() {
            return Err(invalid(
                "verification plan contains a dependency cycle or unknown prerequisite",
            ));
        }
        for id in ready_ids {
            let check = remaining
                .remove(&id)
                .ok_or_else(|| invalid(format!("verification check {id} disappeared")))?;
            dag.insert(node_for_check(
                check,
                commit,
                plan,
                &changed_paths,
                &environment_fingerprint,
                &toolchain_fingerprint,
            ))
            .map_err(invalid)?;
        }
    }
    Ok(dag)
}

fn verifier_build_fingerprint() -> MedusaResult<String> {
    let executable = std::env::current_exe().map_err(|error| {
        MedusaError::new(
            ErrorCode::InternalInvariant,
            ErrorCategory::Internal,
            format!("failed to identify verifier executable: {error}"),
        )
    })?;
    let bytes = fs::read(&executable).map_err(|error| {
        MedusaError::new(
            ErrorCode::InternalInvariant,
            ErrorCategory::Internal,
            format!(
                "failed to fingerprint verifier executable {}: {error}",
                executable.display()
            ),
        )
    })?;
    Ok(fingerprint_bytes(&bytes))
}

fn fingerprint_bytes(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

fn node_for_check(
    check: &VerificationCheck,
    commit: &str,
    plan: &VerificationPlan,
    changed_paths: &BTreeSet<String>,
    environment_fingerprint: &str,
    toolchain_fingerprint: &str,
) -> VerificationNode {
    let command = check.program.as_deref().map_or_else(
        || format!("internal:{:?}", check.kind),
        |program| format!("{} {}", program, check.args.join(" ")),
    );
    VerificationNode {
        id: check.id.clone(),
        command,
        dependencies: dependencies(plan, check),
        authority: VerificationAuthority::IndependentAcceptance,
        expected_duration_ms: 0,
        resource_class: resource_class(check).to_owned(),
        input: VerificationInputKey {
            repository_revision: commit.to_owned(),
            tree_fingerprint: check.input_fingerprint.clone(),
            environment_fingerprint: environment_fingerprint.to_owned(),
            toolchain_fingerprint: toolchain_fingerprint.to_owned(),
            adapter_version: "verification-authority-v1".to_owned(),
            changed_paths: changed_paths.clone(),
        },
        state: VerificationNodeState::Pending,
    }
}

pub(crate) fn execute_command_wave(
    repo: &Path,
    checks: &[&VerificationCheck],
) -> BTreeMap<String, Result<ExecutedVerificationCommand, String>> {
    thread::scope(|scope| {
        let mut handles = Vec::new();
        for check in checks {
            let Some(program) = check.program.as_deref() else {
                continue;
            };
            if matches!(
                check.kind,
                VerificationCheckKind::ArtifactSemantic
                    | VerificationCheckKind::BrowserBehavior
                    | VerificationCheckKind::Accessibility
            ) {
                continue;
            }
            let working_directory = if check.working_directory == "." {
                repo.to_path_buf()
            } else {
                repo.join(&check.working_directory)
            };
            let id = check.id.clone();
            let args = check.args.clone();
            handles.push((
                id,
                scope.spawn(move || {
                    execute_verification_command(&working_directory, program, &args)
                        .map_err(|error| error.to_string())
                }),
            ));
        }
        handles
            .into_iter()
            .map(|(id, handle)| {
                let result = match handle.join() {
                    Ok(result) => result,
                    Err(_) => Err("verification worker terminated unexpectedly".to_owned()),
                };
                (id, result)
            })
            .collect()
    })
}

fn dependencies(plan: &VerificationPlan, check: &VerificationCheck) -> BTreeSet<String> {
    let mut result = BTreeSet::new();
    if check.kind != VerificationCheckKind::Format {
        result.extend(
            plan.checks
                .iter()
                .filter(|candidate| {
                    candidate.kind == VerificationCheckKind::Format
                        && candidate.working_directory == check.working_directory
                })
                .map(|candidate| candidate.id.clone()),
        );
    }
    if matches!(
        check.kind,
        VerificationCheckKind::Integration
            | VerificationCheckKind::BrowserBehavior
            | VerificationCheckKind::Accessibility
            | VerificationCheckKind::Packaging
    ) {
        result.extend(
            plan.checks
                .iter()
                .filter(|candidate| {
                    candidate.kind == VerificationCheckKind::Build
                        && candidate.working_directory == check.working_directory
                })
                .map(|candidate| candidate.id.clone()),
        );
    }
    result
}

fn resource_class(check: &VerificationCheck) -> &'static str {
    match check.kind {
        VerificationCheckKind::Format => "cpu-small",
        VerificationCheckKind::Lint | VerificationCheckKind::Typecheck => "cpu-medium",
        VerificationCheckKind::Unit => "cpu-medium",
        VerificationCheckKind::Integration => "cpu-large",
        VerificationCheckKind::Build => "cpu-large",
        VerificationCheckKind::ArtifactSemantic => "io-small",
        VerificationCheckKind::BrowserBehavior | VerificationCheckKind::Accessibility => "browser",
        VerificationCheckKind::Packaging => "cpu-large",
        VerificationCheckKind::Security => "network-small",
        VerificationCheckKind::RepositoryDefined => "cpu-medium",
    }
}

fn toolchain_fingerprint(repo: &Path) -> String {
    let version = execute_verification_command(repo, "rustc", &["--version".to_owned()])
        .ok()
        .filter(|result| result.passed)
        .map(|result| String::from_utf8_lossy(&result.stdout).trim().to_owned())
        .unwrap_or_else(|| "rustc-unavailable".to_owned());
    fingerprint(&version)
}

fn fingerprint(value: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(value.as_bytes());
    format!("{:x}", hasher.finalize())
}

fn invalid(message: impl Into<String>) -> MedusaError {
    MedusaError::new(
        ErrorCode::InternalInvariant,
        ErrorCategory::Internal,
        message.into(),
    )
}
