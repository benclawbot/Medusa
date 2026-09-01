use std::path::Path;

use medusa_multi_agent_scheduler::{
    RiskLevel, TaskKind,
    mutation_dag::{
        DecompositionDecision, MutationDag, MutationResource, MutationResourceKind,
        MutationTaskContract,
    },
};
use medusa_workers::WorkerManager;

use crate::coordination::{
    multi_agent_coordinator::CoordinatorEvidence,
    production_orchestrator::{AgentContract, AgentRole, ProductionExecutionPlan},
};

const MAX_PARALLEL_MUTATORS: u16 = 3;
const MIN_DECOMPOSITION_CONFIDENCE_MILLI: u16 = 850;

pub(crate) fn repository_revision(repo: &Path) -> Result<String, String> {
    WorkerManager::new(repo, repo.join(".medusa/parallel-planning-worktrees"))
        .map_err(|error| error.to_string())?
        .repository_head()
        .map_err(|error| error.to_string())
}

pub(crate) fn decomposition_for(
    repo: &Path,
    plan: &ProductionExecutionPlan,
    repository_revision: &str,
) -> Result<DecompositionDecision, String> {
    let contract = implementation_contract(plan)?;
    let implementation = plan
        .planning
        .task(TaskKind::Implementation)
        .ok_or_else(|| "parallel mutation planning requires an implementation task".to_owned())?;
    if !crate::workspace_worker_manager::is_git_repository(repo) {
        return Ok(single(
            "directory workspaces use one isolated content-addressed snapshot implementer; conflict-aware parallel mutation currently requires Git worktree staging",
        ));
    }
    let scope = &contract.allowed_write_paths;
    if plan.planning.risk == RiskLevel::High {
        return Ok(single("high-risk mutations remain single-implementer"));
    }
    if scope.len() < 2 || scope.iter().any(|path| path == "repository") {
        return Ok(single(
            "parallel mutation requires at least two exact repository paths",
        ));
    }
    if scope.len() > usize::from(MAX_PARALLEL_MUTATORS) {
        return Ok(single(
            "mutation scope exceeds the bounded parallel worker budget",
        ));
    }
    if repository_revision.trim().is_empty() {
        return Err(
            "parallel mutation planning requires an immutable repository revision".to_owned(),
        );
    }

    let tasks = scope
        .iter()
        .enumerate()
        .map(|(index, path)| {
            let normalized = path.trim().replace('\\', "/");
            if normalized.is_empty() || normalized.starts_with('/') || normalized.contains("../") {
                return Err(format!("parallel mutation scope path is not repository-relative: {path}"));
            }
            let absolute = repo.join(&normalized);
            if absolute.is_dir() {
                return Err(format!(
                    "parallel mutation scope `{normalized}` is a directory; exact file ownership is required"
                ));
            }
            Ok(MutationTaskContract {
                id: format!("implement-{:02}", index + 1),
                repository_revision: repository_revision.to_owned(),
                resources: resources_for_path(&normalized)?,
                dependencies: Vec::new(),
                capabilities: implementation.task.capabilities.clone(),
                required_evidence: contract.required_evidence.clone(),
                verification_responsibility: vec![
                    "targeted worktree verification".to_owned(),
                    "independent runtime verification before integration".to_owned(),
                ],
                confidence_milli: plan.planning.confidence_milli,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;

    MutationDag::build(
        tasks,
        MAX_PARALLEL_MUTATORS,
        MIN_DECOMPOSITION_CONFIDENCE_MILLI,
    )
    .map_err(str::to_owned)
}

pub(crate) fn child_execution(
    plan: &ProductionExecutionPlan,
    preflight: &CoordinatorEvidence,
    task: &MutationTaskContract,
) -> Result<(ProductionExecutionPlan, CoordinatorEvidence), String> {
    let owned_paths = task
        .resources
        .iter()
        .filter(|resource| resource.kind == MutationResourceKind::Path)
        .map(|resource| resource.key.clone())
        .collect::<Vec<_>>();
    if owned_paths.len() != 1 {
        return Err("parallel child execution requires exactly one owned path".to_owned());
    }

    let mut child = plan.clone();
    child.fingerprint = format!("{}:parallel:{}", plan.fingerprint, task.id);
    child.planning.scope.effective = owned_paths.clone();
    if let Some(planned) = child
        .planning
        .tasks
        .iter_mut()
        .find(|planned| planned.kind == TaskKind::Implementation)
    {
        planned.task.write_paths = owned_paths.clone();
        planned.task.speculative = false;
    }
    if let Some(scheduled) = child
        .tasks
        .iter_mut()
        .find(|scheduled| scheduled.id == "implement")
    {
        scheduled.write_paths = owned_paths.clone();
        scheduled.speculative = false;
    }
    let contract = child
        .contracts
        .iter_mut()
        .find(|contract| contract.role == AgentRole::Implementer)
        .ok_or_else(|| "parallel child plan lost implementer contract".to_owned())?;
    contract.allowed_write_paths = owned_paths.clone();
    contract.objective = format!(
        "{}\n\nParallel child ownership: mutate only {:?}. Do not edit any sibling task scope.",
        contract.objective, owned_paths
    );

    let root = preflight
        .state_path
        .parent()
        .ok_or_else(|| "parallel preflight evidence path has no execution root".to_owned())?
        .join("parallel")
        .join(&task.id);
    let child_preflight = CoordinatorEvidence {
        plan_fingerprint: child.fingerprint.clone(),
        repository_fingerprint: preflight.repository_fingerprint.clone(),
        workers: preflight.workers.clone(),
        state_path: root.join("preflight-evidence.json"),
    };
    Ok((child, child_preflight))
}

fn implementation_contract(plan: &ProductionExecutionPlan) -> Result<&AgentContract, String> {
    let implementers = plan
        .contracts
        .iter()
        .filter(|contract| contract.role == AgentRole::Implementer)
        .collect::<Vec<_>>();
    match implementers.as_slice() {
        [contract] => Ok(*contract),
        [] => Err("parallel mutation planning requires an implementer contract".to_owned()),
        _ => Err("parallel mutation planning expected one parent implementer contract".to_owned()),
    }
}

fn resources_for_path(path: &str) -> Result<Vec<MutationResource>, String> {
    let mut resources =
        vec![MutationResource::new(MutationResourceKind::Path, path).map_err(str::to_owned)?];
    let file_name = path.rsplit('/').next().unwrap_or(path);
    let lower = path.to_ascii_lowercase();
    let specialized = if matches!(file_name, "Cargo.toml" | "package.json" | "pyproject.toml") {
        Some(MutationResourceKind::Manifest)
    } else if matches!(
        file_name,
        "Cargo.lock" | "package-lock.json" | "pnpm-lock.yaml" | "yarn.lock"
    ) {
        Some(MutationResourceKind::Lockfile)
    } else if lower.contains("/migrations/") || lower.starts_with("migrations/") {
        Some(MutationResourceKind::Migration)
    } else if lower.contains("snapshot") || lower.ends_with(".snap") {
        Some(MutationResourceKind::Snapshot)
    } else if lower.contains("generated") || lower.contains("/gen/") {
        Some(MutationResourceKind::GeneratedOutput)
    } else {
        None
    };
    if let Some(kind) = specialized {
        resources.push(MutationResource::new(kind, path).map_err(str::to_owned)?);
    }
    Ok(resources)
}

fn single(reason: &str) -> DecompositionDecision {
    DecompositionDecision::SingleImplementer {
        reason: reason.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_hidden_shared_artifacts() {
        let manifest = resources_for_path("crates/api/Cargo.toml").expect("manifest resources");
        assert!(
            manifest
                .iter()
                .any(|resource| resource.kind == MutationResourceKind::Manifest)
        );
        let generated = resources_for_path("src/generated/client.rs").expect("generated resources");
        assert!(
            generated
                .iter()
                .any(|resource| resource.kind == MutationResourceKind::GeneratedOutput)
        );
    }

    #[test]
    fn lockfiles_and_migrations_are_specialized() {
        let lockfile = resources_for_path("Cargo.lock").expect("lockfile resources");
        assert!(
            lockfile
                .iter()
                .any(|resource| resource.kind == MutationResourceKind::Lockfile)
        );
        let migration = resources_for_path("db/migrations/001.sql").expect("migration resources");
        assert!(
            migration
                .iter()
                .any(|resource| resource.kind == MutationResourceKind::Migration)
        );
    }
}
