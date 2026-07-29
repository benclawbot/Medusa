use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use medusa_multi_agent_scheduler::{schedule, Schedule, Task, Worker};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{prompt::PromptDraft, RuntimeActivity, RuntimeActivityKind, RuntimeEvent};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum ExecutionMode {
    Direct,
    Orchestrated,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum AgentRole {
    Planner,
    Researcher,
    Implementer,
    Reviewer,
    Verifier,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DelegationPolicy {
    pub allowed: bool,
    pub max_depth: u8,
    pub max_parallel_subagents: u16,
    pub parent_must_review: bool,
    pub parent_must_integrate: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AgentContract {
    pub task_id: String,
    pub role: AgentRole,
    pub objective: String,
    pub dependencies: Vec<String>,
    pub allowed_write_paths: Vec<String>,
    pub required_evidence: Vec<String>,
    pub delegation: DelegationPolicy,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ContextPacket {
    pub task_id: String,
    pub objective: String,
    pub repository_scope: Vec<String>,
    pub dependency_outputs: BTreeMap<String, String>,
    pub authoritative_policies: Vec<String>,
    pub acceptance_criteria: Vec<String>,
    pub contract: AgentContract,
    pub fingerprint: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ProductionExecutionPlan {
    pub mode: ExecutionMode,
    pub tasks: Vec<Task>,
    pub schedule: Option<Schedule>,
    pub contracts: Vec<AgentContract>,
    pub fingerprint: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PersistedOutcome {
    pub objective_fingerprint: String,
    pub plan_fingerprint: String,
    pub verified: bool,
    pub failed: bool,
}

pub fn plan(draft: &PromptDraft) -> Result<ProductionExecutionPlan, &'static str> {
    let text = draft.text.trim();
    let mode = classify(text, draft.attachments.len());
    let tasks = if mode == ExecutionMode::Orchestrated {
        decompose(text)
    } else {
        Vec::new()
    };
    let schedule = if tasks.is_empty() {
        None
    } else {
        Some(schedule(tasks.clone(), default_workers())?)
    };
    let contracts = tasks.iter().map(|task| contract_for(text, task)).collect();
    let fingerprint = digest(&(mode, &tasks, &schedule, &contracts));
    Ok(ProductionExecutionPlan {
        mode,
        tasks,
        schedule,
        contracts,
        fingerprint,
    })
}

pub fn runtime_context(plan: &ProductionExecutionPlan) -> String {
    if plan.mode == ExecutionMode::Direct {
        return "Production execution mode: direct. Do not create unnecessary worker or subagent overhead for this conversational or single-step objective.".to_owned();
    }
    let contracts = plan
        .contracts
        .iter()
        .map(|contract| {
            format!(
                "- {} ({:?}): dependencies={:?}; writes={:?}; required evidence={:?}",
                contract.task_id,
                contract.role,
                contract.dependencies,
                contract.allowed_write_paths,
                contract.required_evidence,
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "Production execution mode: coordinated. Independent read-only teammates are dispatched with durable leases, role-bound runtime policy, isolated sessions, durable mailboxes, and evidence handoff. The parent AgentEngine remains the only mutating authority until worktree-isolated implementers are enabled. Repository verification is the completion gate.\n{contracts}"
    )
}

pub fn context_for_task(
    plan: &ProductionExecutionPlan,
    task_id: &str,
    dependency_outputs: BTreeMap<String, String>,
    policies: Vec<String>,
    acceptance_criteria: Vec<String>,
) -> Result<ContextPacket, &'static str> {
    let contract = plan
        .contracts
        .iter()
        .find(|contract| contract.task_id == task_id)
        .cloned()
        .ok_or("task contract does not exist")?;
    if dependency_outputs
        .keys()
        .any(|dependency| !contract.dependencies.contains(dependency))
    {
        return Err("context contains output from an unrelated dependency");
    }
    if contract
        .dependencies
        .iter()
        .any(|dependency| !dependency_outputs.contains_key(dependency))
    {
        return Err("context is missing a required dependency output");
    }
    let objective = contract.objective.clone();
    let repository_scope = contract.allowed_write_paths.clone();
    let fingerprint = digest(&(
        task_id,
        &objective,
        &repository_scope,
        &dependency_outputs,
        &policies,
        &acceptance_criteria,
        &contract,
    ));
    Ok(ContextPacket {
        task_id: task_id.to_owned(),
        objective,
        repository_scope,
        dependency_outputs,
        authoritative_policies: policies,
        acceptance_criteria,
        contract,
        fingerprint,
    })
}

pub fn validate_subagent_result(
    parent: &ContextPacket,
    delegated_task_id: &str,
    claimed_parent_fingerprint: &str,
    evidence: &[String],
) -> Result<(), &'static str> {
    if !parent.contract.delegation.allowed {
        return Err("subagent delegation is not allowed for this task");
    }
    if delegated_task_id.trim().is_empty() {
        return Err("delegated task identifier cannot be empty");
    }
    if claimed_parent_fingerprint != parent.fingerprint {
        return Err("subagent result was produced from stale or unrelated context");
    }
    if evidence.is_empty() || evidence.iter().any(|item| item.trim().is_empty()) {
        return Err("subagent result requires non-empty evidence for parent review");
    }
    Ok(())
}

pub fn events(plan: &ProductionExecutionPlan) -> Vec<RuntimeEvent> {
    match (&plan.mode, &plan.schedule) {
        (ExecutionMode::Direct, _) => vec![RuntimeEvent::Activity(RuntimeActivity {
            id: Some(plan.fingerprint.clone()),
            kind: RuntimeActivityKind::Progress,
            title: "Direct execution selected".to_owned(),
            details: vec![
                "The objective is conversational or single-step; orchestration planning was skipped."
                    .to_owned(),
            ],
        })],
        (ExecutionMode::Orchestrated, Some(schedule)) => {
            vec![RuntimeEvent::Activity(RuntimeActivity {
                id: Some(plan.fingerprint.clone()),
                kind: RuntimeActivityKind::Progress,
                title: "Execution contracts prepared".to_owned(),
                details: vec![
                    format!("{} dependency-aware task contracts", plan.tasks.len()),
                    format!("{} dependency-aware schedule waves", schedule.waves.len()),
                    format!("{} independent tasks are eligible for the first dispatch wave", schedule.waves.first().map_or(0, Vec::len)),
                    "The parent remains the sole mutation and integration authority in this slice."
                        .to_owned(),
                    "Repository verification remains the completion gate.".to_owned(),
                ],
            })]
        }
        _ => Vec::new(),
    }
}

pub fn persist_outcome(
    repo: &Path,
    draft: &PromptDraft,
    plan: &ProductionExecutionPlan,
    verified: bool,
    failed: bool,
) -> std::io::Result<PathBuf> {
    let directory = repo.join(".medusa").join("learning");
    fs::create_dir_all(&directory)?;
    let path = directory.join("runtime-outcomes.jsonl");
    let outcome = PersistedOutcome {
        objective_fingerprint: digest(&draft.text),
        plan_fingerprint: plan.fingerprint.clone(),
        verified,
        failed,
    };
    let mut line = serde_json::to_vec(&outcome).map_err(std::io::Error::other)?;
    line.push(b'\n');
    use std::io::Write;
    let mut file = fs::OpenOptions::new().create(true).append(true).open(&path)?;
    file.write_all(&line)?;
    Ok(path)
}

fn contract_for(objective: &str, task: &Task) -> AgentContract {
    let (role, required_evidence) = match task.id.as_str() {
        "analyze" => (
            AgentRole::Planner,
            vec!["repository evidence".to_owned(), "dependency-aware plan".to_owned()],
        ),
        "risk-review" => (
            AgentRole::Researcher,
            vec!["risk inventory".to_owned(), "failure-mode evidence".to_owned()],
        ),
        "implement" => (
            AgentRole::Implementer,
            vec!["patch or commit evidence".to_owned(), "focused tests".to_owned()],
        ),
        "review" => (
            AgentRole::Reviewer,
            vec!["conflict analysis".to_owned(), "policy compliance".to_owned()],
        ),
        _ => (
            AgentRole::Verifier,
            vec!["acceptance criteria".to_owned(), "repository verification".to_owned()],
        ),
    };
    AgentContract {
        task_id: task.id.clone(),
        role,
        objective: objective.to_owned(),
        dependencies: task.dependencies.clone(),
        allowed_write_paths: task.write_paths.clone(),
        required_evidence,
        delegation: DelegationPolicy {
            allowed: matches!(role, AgentRole::Planner | AgentRole::Researcher | AgentRole::Reviewer | AgentRole::Verifier),
            max_depth: 0,
            max_parallel_subagents: 0,
            parent_must_review: true,
            parent_must_integrate: true,
        },
    }
}

fn classify(text: &str, attachments: usize) -> ExecutionMode {
    let lower = text.to_ascii_lowercase();
    let complex_markers = [
        "implement", "fix", "refactor", "repository", "codebase", "tests", "ci", "multiple",
        "all open", "across", "architecture", "migration", "release",
    ];
    let simple_markers = ["hello", "thanks", "thank you", "explain", "what is", "summarize"];
    if attachments > 1
        || text.lines().count() > 3
        || text.len() > 220
        || complex_markers.iter().any(|marker| lower.contains(marker))
    {
        ExecutionMode::Orchestrated
    } else if simple_markers.iter().any(|marker| lower.starts_with(marker))
        || text.split_whitespace().count() <= 12
    {
        ExecutionMode::Direct
    } else {
        ExecutionMode::Orchestrated
    }
}

fn decompose(text: &str) -> Vec<Task> {
    let scope = infer_scope(text);
    vec![
        Task { id: "analyze".to_owned(), dependencies: vec![], capabilities: vec!["analysis".to_owned()], write_paths: vec![], speculative: false },
        Task { id: "risk-review".to_owned(), dependencies: vec![], capabilities: vec!["risk-review".to_owned()], write_paths: vec![], speculative: false },
        Task { id: "implement".to_owned(), dependencies: vec!["analyze".to_owned(), "risk-review".to_owned()], capabilities: vec!["coding".to_owned()], write_paths: vec![scope], speculative: false },
        Task { id: "review".to_owned(), dependencies: vec!["implement".to_owned()], capabilities: vec!["review".to_owned()], write_paths: vec![], speculative: false },
        Task { id: "verify".to_owned(), dependencies: vec!["review".to_owned()], capabilities: vec!["verification".to_owned()], write_paths: vec![], speculative: false },
    ]
}

fn infer_scope(text: &str) -> String {
    text.split_whitespace()
        .find(|value| value.contains('/'))
        .map(|value| value.trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '/' && c != '-' && c != '_').to_owned())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "repository".to_owned())
}

fn default_workers() -> Vec<Worker> {
    vec![
        Worker { id: "planner".to_owned(), capabilities: vec!["analysis".to_owned()], healthy: true, capacity: 1 },
        Worker { id: "risk-reviewer".to_owned(), capabilities: vec!["risk-review".to_owned()], healthy: true, capacity: 1 },
        Worker { id: "coder".to_owned(), capabilities: vec!["coding".to_owned()], healthy: true, capacity: 1 },
        Worker { id: "reviewer".to_owned(), capabilities: vec!["review".to_owned()], healthy: true, capacity: 1 },
        Worker { id: "verifier".to_owned(), capabilities: vec!["verification".to_owned()], healthy: true, capacity: 1 },
    ]
}

fn digest<T: Serialize>(value: &T) -> String {
    let bytes = serde_json::to_vec(value).unwrap_or_default();
    format!("{:x}", Sha256::digest(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simple_conversation_avoids_orchestration() {
        let draft = PromptDraft { text: "Hello, explain this concept".to_owned(), ..PromptDraft::default() };
        assert_eq!(plan(&draft).unwrap().mode, ExecutionMode::Direct);
    }

    #[test]
    fn coding_objective_is_decomposed_and_scheduled() {
        let draft = PromptDraft { text: "Implement a repository-wide refactor and run all tests and CI".to_owned(), ..PromptDraft::default() };
        let planned = plan(&draft).unwrap();
        assert_eq!(planned.mode, ExecutionMode::Orchestrated);
        assert_eq!(planned.tasks.len(), 5);
        assert_eq!(planned.schedule.as_ref().unwrap().waves.len(), 4);
        assert_eq!(planned.schedule.as_ref().unwrap().waves[0].len(), 2);
        assert!(planned.contracts.iter().any(|contract| contract.delegation.allowed));
    }

    #[test]
    fn task_context_contains_only_declared_dependencies() {
        let draft = PromptDraft { text: "Fix repository tests".to_owned(), ..PromptDraft::default() };
        let planned = plan(&draft).unwrap();
        let packet = context_for_task(
            &planned,
            "implement",
            BTreeMap::from([
                ("analyze".to_owned(), "analysis evidence".to_owned()),
                ("risk-review".to_owned(), "risk evidence".to_owned()),
            ]),
            vec!["path policy".to_owned()],
            vec!["tests pass".to_owned()],
        ).unwrap();
        assert_eq!(packet.dependency_outputs.len(), 2);
        assert!(!packet.contract.delegation.allowed);
    }

    #[test]
    fn active_delegation_accepts_bound_evidence() {
        let draft = PromptDraft { text: "Fix repository tests".to_owned(), ..PromptDraft::default() };
        let planned = plan(&draft).unwrap();
        let packet = context_for_task(&planned, "analyze", BTreeMap::new(), vec![], vec![]).unwrap();
        assert!(validate_subagent_result(&packet, "analyze", &packet.fingerprint, &["evidence".to_owned()]).is_ok());
        assert!(validate_subagent_result(&packet, "analyze", "stale", &["evidence".to_owned()]).is_err());
    }

    #[test]
    fn runtime_context_and_events_describe_real_dispatch() {
        let draft = PromptDraft { text: "Implement a repository-wide refactor".to_owned(), ..PromptDraft::default() };
        let planned = plan(&draft).unwrap();
        let context = runtime_context(&planned);
        assert!(context.contains("Independent read-only teammates are dispatched"));
        let rendered = format!("{:?}", events(&planned));
        assert!(rendered.contains("independent tasks are eligible"));
        assert!(!rendered.contains("no workers were dispatched"));
    }

    #[test]
    fn outcomes_are_persisted() {
        let directory = tempfile::tempdir().unwrap();
        let draft = PromptDraft { text: "Fix tests".to_owned(), ..PromptDraft::default() };
        let planned = plan(&draft).unwrap();
        let path = persist_outcome(directory.path(), &draft, &planned, true, false).unwrap();
        let value = fs::read_to_string(path).unwrap();
        assert!(value.contains("\"verified\":true"));
    }
}
