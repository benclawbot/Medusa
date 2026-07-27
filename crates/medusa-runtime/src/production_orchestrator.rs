use std::{
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

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ProductionExecutionPlan {
    pub mode: ExecutionMode,
    pub tasks: Vec<Task>,
    pub schedule: Option<Schedule>,
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
    let fingerprint = digest(&(mode, &tasks, &schedule));
    Ok(ProductionExecutionPlan {
        mode,
        tasks,
        schedule,
        fingerprint,
    })
}

pub fn events(plan: &ProductionExecutionPlan) -> Vec<RuntimeEvent> {
    match (&plan.mode, &plan.schedule) {
        (ExecutionMode::Direct, _) => vec![RuntimeEvent::Activity(RuntimeActivity {
            id: Some(plan.fingerprint.clone()),
            kind: RuntimeActivityKind::Progress,
            title: "Direct execution selected".to_owned(),
            details: vec![
                "The objective is conversational or single-step; multi-agent overhead was skipped."
                    .to_owned(),
            ],
        })],
        (ExecutionMode::Orchestrated, Some(schedule)) => {
            let mut result = vec![RuntimeEvent::Activity(RuntimeActivity {
                id: Some(plan.fingerprint.clone()),
                kind: RuntimeActivityKind::Progress,
                title: "Production orchestrator planned execution".to_owned(),
                details: vec![
                    format!("{} dependency-aware tasks", plan.tasks.len()),
                    format!("{} bounded-concurrency waves", schedule.waves.len()),
                    "Repository verification remains the completion gate.".to_owned(),
                ],
            })];
            for (index, wave) in schedule.waves.iter().enumerate() {
                result.push(RuntimeEvent::Activity(RuntimeActivity {
                    id: Some(format!("{}-wave-{}", plan.fingerprint, index + 1)),
                    kind: RuntimeActivityKind::Tool,
                    title: format!("Dispatch wave {}", index + 1),
                    details: wave
                        .iter()
                        .map(|assignment| {
                            format!("{} -> {}", assignment.task_id, assignment.worker_id)
                        })
                        .collect(),
                }));
            }
            result
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
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?;
    file.write_all(&line)?;
    Ok(path)
}

fn classify(text: &str, attachments: usize) -> ExecutionMode {
    let lower = text.to_ascii_lowercase();
    let complex_markers = [
        "implement",
        "fix",
        "refactor",
        "repository",
        "codebase",
        "tests",
        "ci",
        "multiple",
        "all open",
        "across",
        "architecture",
        "migration",
        "release",
    ];
    let simple_markers = [
        "hello",
        "thanks",
        "thank you",
        "explain",
        "what is",
        "summarize",
    ];
    if attachments > 1
        || text.lines().count() > 3
        || text.len() > 220
        || complex_markers.iter().any(|marker| lower.contains(marker))
    {
        ExecutionMode::Orchestrated
    } else if simple_markers
        .iter()
        .any(|marker| lower.starts_with(marker))
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
        Task {
            id: "analyze".to_owned(),
            dependencies: vec![],
            capabilities: vec!["analysis".to_owned()],
            write_paths: vec![],
            speculative: false,
        },
        Task {
            id: "implement".to_owned(),
            dependencies: vec!["analyze".to_owned()],
            capabilities: vec!["coding".to_owned()],
            write_paths: vec![scope],
            speculative: false,
        },
        Task {
            id: "review".to_owned(),
            dependencies: vec!["implement".to_owned()],
            capabilities: vec!["review".to_owned()],
            write_paths: vec![],
            speculative: false,
        },
        Task {
            id: "verify".to_owned(),
            dependencies: vec!["review".to_owned()],
            capabilities: vec!["verification".to_owned()],
            write_paths: vec![],
            speculative: false,
        },
    ]
}

fn infer_scope(text: &str) -> String {
    text.split_whitespace()
        .find(|value| value.contains('/'))
        .map(|value| {
            value
                .trim_matches(|c: char| {
                    !c.is_ascii_alphanumeric() && c != '/' && c != '-' && c != '_'
                })
                .to_owned()
        })
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "repository".to_owned())
}

fn default_workers() -> Vec<Worker> {
    vec![
        Worker {
            id: "planner".to_owned(),
            capabilities: vec!["analysis".to_owned()],
            healthy: true,
            capacity: 1,
        },
        Worker {
            id: "coder".to_owned(),
            capabilities: vec!["coding".to_owned()],
            healthy: true,
            capacity: 2,
        },
        Worker {
            id: "reviewer".to_owned(),
            capabilities: vec!["review".to_owned()],
            healthy: true,
            capacity: 1,
        },
        Worker {
            id: "verifier".to_owned(),
            capabilities: vec!["verification".to_owned()],
            healthy: true,
            capacity: 1,
        },
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
        let draft = PromptDraft {
            text: "Hello, explain this concept".to_owned(),
            ..PromptDraft::default()
        };
        assert_eq!(plan(&draft).unwrap().mode, ExecutionMode::Direct);
    }

    #[test]
    fn coding_objective_is_decomposed_and_scheduled() {
        let draft = PromptDraft {
            text: "Implement a repository-wide refactor and run all tests and CI".to_owned(),
            ..PromptDraft::default()
        };
        let planned = plan(&draft).unwrap();
        assert_eq!(planned.mode, ExecutionMode::Orchestrated);
        assert_eq!(planned.tasks.len(), 4);
        assert_eq!(planned.schedule.as_ref().unwrap().waves.len(), 4);
    }

    #[test]
    fn outcomes_are_persisted() {
        let directory = tempfile::tempdir().unwrap();
        let draft = PromptDraft {
            text: "Fix tests".to_owned(),
            ..PromptDraft::default()
        };
        let planned = plan(&draft).unwrap();
        let path = persist_outcome(directory.path(), &draft, &planned, true, false).unwrap();
        let value = fs::read_to_string(path).unwrap();
        assert!(value.contains("\"verified\":true"));
    }
}
