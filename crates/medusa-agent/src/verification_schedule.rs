use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
    thread,
    time::Instant,
};

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use medusa_evidence::{VerificationCheck, VerificationCheckKind, VerificationPlan};
use sha2::{Digest, Sha256};

use crate::{
    verification::{
        ExecutedVerificationCommand, execute_verification_command,
        execute_verification_command_cancellable,
    },
    verification_authority::verification_cancellation::active_verification_cancellation,
    verification_dag::{
        VerificationAuthority, VerificationDag, VerificationInputKey, VerificationNode,
        VerificationNodeState,
    },
};

#[derive(Debug)]
pub(crate) struct VerificationWaveExecution {
    pub results: BTreeMap<String, Result<ExecutedVerificationCommand, String>>,
    pub queue_duration_ms: BTreeMap<String, u64>,
    pub wall_duration_ms: u64,
}

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
    let verifier_build = if persistent_reuse_allowed(plan) {
        verifier_build_fingerprint()?
    } else {
        "not-persistently-reusable".to_owned()
    };
    let environment_fingerprint = fingerprint(&format!(
        "{}:{}:{}",
        std::env::consts::OS,
        std::env::consts::ARCH,
        verifier_build
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

fn persistent_reuse_allowed(plan: &VerificationPlan) -> bool {
    !plan.checks.is_empty()
        && plan.checks.iter().all(|check| {
            check.kind == VerificationCheckKind::ArtifactSemantic && check.program.is_none()
        })
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
        expected_duration_ms: expected_duration_ms(check.kind),
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
) -> VerificationWaveExecution {
    let wave_started = Instant::now();
    let (results, queue_duration_ms) = thread::scope(|scope| {
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
                    let queue_duration_ms = wave_started.elapsed().as_millis() as u64;
                    let result = if let Some(cancellation) =
                        active_verification_cancellation(&working_directory)
                    {
                        execute_verification_command_cancellable(
                            &working_directory,
                            program,
                            &args,
                            &cancellation,
                        )
                        .map_err(|error| error.to_string())
                    } else {
                        execute_verification_command(&working_directory, program, &args)
                            .map_err(|error| error.to_string())
                    };
                    (queue_duration_ms, result)
                }),
            ));
        }
        let mut results = BTreeMap::new();
        let mut queue_duration_ms = BTreeMap::new();
        for (id, handle) in handles {
            let (queue_ms, result) = match handle.join() {
                Ok(result) => result,
                Err(_) => (
                    wave_started.elapsed().as_millis() as u64,
                    Err("verification worker terminated unexpectedly".to_owned()),
                ),
            };
            queue_duration_ms.insert(id.clone(), queue_ms);
            results.insert(id, result);
        }
        (results, queue_duration_ms)
    });
    VerificationWaveExecution {
        results,
        queue_duration_ms,
        wall_duration_ms: wave_started.elapsed().as_millis() as u64,
    }
}

pub(crate) fn critical_path_duration_ms(
    plan: &VerificationPlan,
    durations: &BTreeMap<String, u64>,
) -> u64 {
    let mut remaining = plan
        .checks
        .iter()
        .map(|check| (check.id.clone(), check))
        .collect::<BTreeMap<_, _>>();
    let mut totals = BTreeMap::<String, u64>::new();
    while !remaining.is_empty() {
        let ready_ids = remaining
            .iter()
            .filter_map(|(id, check)| {
                dependencies(plan, check)
                    .iter()
                    .all(|dependency| totals.contains_key(dependency))
                    .then_some(id.clone())
            })
            .collect::<Vec<_>>();
        if ready_ids.is_empty() {
            return 0;
        }
        for id in ready_ids {
            let Some(check) = remaining.remove(&id) else {
                continue;
            };
            let prerequisite_ms = dependencies(plan, check)
                .iter()
                .filter_map(|dependency| totals.get(dependency))
                .copied()
                .max()
                .unwrap_or(0);
            totals.insert(
                id.clone(),
                prerequisite_ms.saturating_add(durations.get(&id).copied().unwrap_or(0)),
            );
        }
    }
    totals.values().copied().max().unwrap_or(0)
}

fn expected_duration_ms(kind: VerificationCheckKind) -> u64 {
    match kind {
        VerificationCheckKind::Format => 500,
        VerificationCheckKind::Lint | VerificationCheckKind::Typecheck => 5_000,
        VerificationCheckKind::Unit => 10_000,
        VerificationCheckKind::Integration => 30_000,
        VerificationCheckKind::Build => 30_000,
        VerificationCheckKind::ArtifactSemantic => 500,
        VerificationCheckKind::BrowserBehavior | VerificationCheckKind::Accessibility => 15_000,
        VerificationCheckKind::Packaging => 30_000,
        VerificationCheckKind::Security => 15_000,
        VerificationCheckKind::RepositoryDefined => 10_000,
    }
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

#[cfg(test)]
mod tests {
    use medusa_evidence::{ChangeKind, ChangedComponent};

    use super::*;

    fn check(id: &str, kind: VerificationCheckKind, working_directory: &str) -> VerificationCheck {
        VerificationCheck {
            id: id.to_owned(),
            kind,
            required: true,
            reason: "test".to_owned(),
            program: Some("rustc".to_owned()),
            args: vec!["--version".to_owned()],
            working_directory: working_directory.to_owned(),
            input_fingerprint: format!("input-{id}"),
        }
    }

    #[test]
    fn production_nodes_have_nonzero_expected_durations() {
        for kind in [
            VerificationCheckKind::Format,
            VerificationCheckKind::Lint,
            VerificationCheckKind::Typecheck,
            VerificationCheckKind::Unit,
            VerificationCheckKind::Integration,
            VerificationCheckKind::Build,
            VerificationCheckKind::ArtifactSemantic,
            VerificationCheckKind::BrowserBehavior,
            VerificationCheckKind::Accessibility,
            VerificationCheckKind::Packaging,
            VerificationCheckKind::Security,
            VerificationCheckKind::RepositoryDefined,
        ] {
            assert!(expected_duration_ms(kind) > 0);
        }
    }

    #[test]
    fn critical_path_uses_dependency_chain_not_serial_sum() {
        let component = ChangedComponent::new(ChangeKind::Modified, "src/lib.rs").expect("component");
        let plan = VerificationPlan {
            repository_fingerprint: "repo".to_owned(),
            commit: "commit".to_owned(),
            components: vec![component],
            checks: vec![
                check("format", VerificationCheckKind::Format, "."),
                check("build", VerificationCheckKind::Build, "."),
                check("unit", VerificationCheckKind::Unit, "."),
                check("integration", VerificationCheckKind::Integration, "."),
            ],
            exemptions: Vec::new(),
            fingerprint: "plan".to_owned(),
        };
        let durations = BTreeMap::from([
            ("format".to_owned(), 5),
            ("build".to_owned(), 20),
            ("unit".to_owned(), 10),
            ("integration".to_owned(), 30),
        ]);

        assert_eq!(critical_path_duration_ms(&plan, &durations), 55);
    }
}
