//! Shared runtime integration for explainable learned-behavior retrieval.

use std::{
    collections::BTreeSet,
    env, fs,
    fs::OpenOptions,
    io::Write,
    path::{Path, PathBuf},
    sync::mpsc::Sender,
};

use medusa_improvement::{
    learning_admission::LearningAdmissionPolicy,
    learning_monitor::LearningMonitorStore,
    refinement_authority::{SelectionContext, SelectionResult},
    scoped_memory::RepositoryIdentity,
};
use medusa_core::hidden_command;
use serde::Serialize;
use sha2::{Digest, Sha256};
use time::OffsetDateTime;

use crate::{RuntimeEvent, prompt::PromptDraft};

#[derive(Clone, Debug, Default)]
pub struct RuntimeLearningContext {
    pub prompt_context: Option<String>,
}

#[derive(Serialize)]
struct SelectionAudit<'a> {
    schema_version: u32,
    recorded_at_unix_ms: i64,
    objective_digest: String,
    task_kind: &'a str,
    artifact_kind: &'a str,
    result: &'a SelectionResult,
}

pub fn select(
    repo: &Path,
    draft: &PromptDraft,
    session_id: Option<&str>,
    events: &Sender<RuntimeEvent>,
) -> RuntimeLearningContext {
    let policy = match LearningAdmissionPolicy::for_repository(repo) {
        Ok(policy) => policy,
        Err(error) => {
            let _ = events.send(RuntimeEvent::Notice {
                title: "Learned behavior unavailable".to_owned(),
                details: vec![format!(
                    "Learning privacy state could not be resolved; retrieval failed closed: {error}"
                )],
            });
            return RuntimeLearningContext::default();
        }
    };
    if !policy.capture_enabled() {
        return RuntimeLearningContext::default();
    }

    let objective = draft.text.trim();
    let task_kind = infer_task_kind(objective);
    let artifact_kind = infer_artifact_kind(draft, objective);
    let now_unix_ms = now_unix_ms();
    let authority = match crate::learning_authority::open(repo) {
        Ok(authority) => authority,
        Err(error) => {
            let _ = events.send(RuntimeEvent::Notice {
                title: "Canonical learned behavior unavailable".to_owned(),
                details: vec![format!("selection failed closed: {error}")],
            });
            return RuntimeLearningContext::default();
        }
    };
    let context = SelectionContext {
        repository: repository_identity(repo),
        user_id: owner_id(),
        session_id: session_id.map(str::to_owned),
        task_kind: Some(task_kind.to_owned()),
        // The inferred task artifact (`source_code`, `document`, etc.) is a runtime output
        // category, not the canonical refinement artifact enum. Leaving this filter unset keeps
        // workflow, prompt, memory, and repository refinements eligible when their objective
        // predicate matches instead of silently rejecting every non-repository refinement.
        artifact_kind: None,
        context_tags: context_tags(objective, task_kind, artifact_kind),
        explicit_exclusions: explicit_exclusions(objective),
        objective: objective.to_owned(),
        now_unix_ms,
    };
    let result = match authority.select(&context) {
        Ok(result) => result,
        Err(error) => {
            let _ = events.send(RuntimeEvent::Notice {
                title: "Canonical learned behavior unavailable".to_owned(),
                details: vec![format!(
                    "Selection failed closed; no learned behavior was applied: {error}"
                )],
            });
            return RuntimeLearningContext::default();
        }
    };

    emit_notices(&result, events);
    let projection_revision = authority
        .snapshot()
        .map(|snapshot| snapshot.revision)
        .unwrap_or_default();
    if let Err(error) = LearningMonitorStore::record_selection(
        repo,
        &context,
        &result,
        projection_revision,
        now_unix_ms,
    ) {
        let _ = events.send(RuntimeEvent::Notice {
            title: "Learning effectiveness monitor unavailable".to_owned(),
            details: vec![format!(
                "selection was applied, but its exposure audit was not persisted: {error}"
            )],
        });
    }
    if policy.telemetry_enabled()
        && let Err(error) = append_audit(
            repo,
            objective,
            task_kind,
            artifact_kind,
            now_unix_ms,
            &result,
        )
    {
        let _ = events.send(RuntimeEvent::Notice {
            title: "Learning selection audit unavailable".to_owned(),
            details: vec![error.to_string()],
        });
    }

    RuntimeLearningContext {
        prompt_context: result.prompt_context(),
    }
}

fn emit_notices(result: &SelectionResult, events: &Sender<RuntimeEvent>) {
    if !result.selected.is_empty() {
        let details = result
            .selected
            .iter()
            .map(|selected| {
                format!(
                    "{} v{} (receipt {}): {}",
                    selected.proposal.id,
                    selected.proposal.version,
                    selected.approval_receipt_id,
                    selected.selection_rationale
                )
            })
            .collect();
        let _ = events.send(RuntimeEvent::Notice {
            title: "Learned behavior applied".to_owned(),
            details,
        });
    }

    if !result.blocked_conflicts.is_empty() {
        let _ = events.send(RuntimeEvent::Notice {
            title: "Conflicting learned behavior blocked".to_owned(),
            details: result.blocked_conflicts.clone(),
        });
    }
}

fn append_audit(
    repo: &Path,
    objective: &str,
    task_kind: &str,
    artifact_kind: &str,
    recorded_at_unix_ms: i64,
    result: &SelectionResult,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let path = repo.join(".medusa/learning-selection-audit.jsonl");
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let record = SelectionAudit {
        schema_version: 1,
        recorded_at_unix_ms,
        objective_digest: hex::encode(Sha256::digest(objective.as_bytes())),
        task_kind,
        artifact_kind,
        result,
    };
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    serde_json::to_writer(&mut file, &record)?;
    file.write_all(b"\n")?;
    file.sync_data()?;
    Ok(())
}

fn owner_id() -> String {
    env::var("MEDUSA_USER_ID").unwrap_or_else(|_| "local-user".to_owned())
}

fn repository_identity(repo: &Path) -> Option<RepositoryIdentity> {
    let origin = read_origin(repo).or_else(|| repository_root_commit(repo))?;
    // RepositoryIdentity uses the origin fingerprint for logical identity. The second component is
    // clone/worktree-local and is deliberately based on the git common directory so worktrees of
    // one clone remain correlated without substituting an absolute worktree path for repository
    // identity.
    let common = git_common_directory(repo).unwrap_or_else(|| repo.join(".git"));
    RepositoryIdentity::new(&origin, &common.to_string_lossy()).ok()
}

fn repository_root_commit(repo: &Path) -> Option<String> {
    let output = hidden_command("git")
        .args(["rev-list", "--max-parents=0", "HEAD"])
        .current_dir(repo)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let mut roots = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .collect::<Vec<_>>();
    roots.sort();
    (!roots.is_empty()).then(|| format!("git-roots:{}", roots.join(",")))
}

fn read_origin(repo: &Path) -> Option<String> {
    let common = git_common_directory(repo)?;
    let config = fs::read_to_string(common.join("config")).ok()?;
    let mut in_origin = false;
    for line in config.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            in_origin = trimmed.eq_ignore_ascii_case("[remote \"origin\"]");
        } else if in_origin
            && let Some(value) = trimmed.strip_prefix("url")
        {
            return value
                .split_once('=')
                .map(|(_, origin)| origin.trim().to_owned());
        }
    }
    None
}

fn git_common_directory(repo: &Path) -> Option<PathBuf> {
    let dot_git = repo.join(".git");
    if dot_git.is_dir() {
        return Some(dot_git);
    }
    let pointer = fs::read_to_string(&dot_git).ok()?;
    let git_dir = pointer.trim().strip_prefix("gitdir:")?.trim();
    let git_dir = if Path::new(git_dir).is_absolute() {
        PathBuf::from(git_dir)
    } else {
        repo.join(git_dir)
    };
    let common = fs::read_to_string(git_dir.join("commondir")).ok();
    Some(match common {
        Some(common) => git_dir.join(common.trim()),
        None => git_dir,
    })
}

fn infer_task_kind(objective: &str) -> &'static str {
    let lower = objective.to_ascii_lowercase();
    if contains_any(&lower, &["test", "verify", "validation", "benchmark"]) {
        "testing"
    } else if contains_any(&lower, &["release", "deploy", "publish", "package"]) {
        "release"
    } else if contains_any(&lower, &["plan", "design", "architecture", "research"]) {
        "planning"
    } else if contains_any(&lower, &["review", "audit", "inspect", "triage"]) {
        "review"
    } else if contains_any(&lower, &["format", "formatter", "lint"]) {
        "formatting"
    } else if contains_any(&lower, &["fix", "bug", "implement", "change", "refactor"]) {
        "coding"
    } else {
        "general"
    }
}

fn infer_artifact_kind(draft: &PromptDraft, objective: &str) -> &'static str {
    let lower = objective.to_ascii_lowercase();
    if !draft.attachments.is_empty() {
        "multimodal_prompt"
    } else if contains_any(
        &lower,
        &["documentation", "docs", "readme", "memo", "report"],
    ) {
        "document"
    } else if contains_any(&lower, &["test", "fixture", "benchmark"]) {
        "test"
    } else if contains_any(&lower, &["config", "configuration", "toml", "yaml", "json"]) {
        "configuration"
    } else {
        "source_code"
    }
}

fn context_tags(objective: &str, task_kind: &str, artifact_kind: &str) -> BTreeSet<String> {
    let mut tags = normalized_terms(objective);
    tags.insert(task_kind.to_owned());
    tags.insert(artifact_kind.to_owned());
    tags
}

fn explicit_exclusions(objective: &str) -> BTreeSet<String> {
    let terms = objective
        .split(|character: char| !character.is_ascii_alphanumeric() && character != '@')
        .filter(|term| !term.is_empty())
        .map(str::to_ascii_lowercase)
        .collect::<Vec<_>>();
    let mut exclusions = marker_values(objective, "@exclude-learning:");
    for (index, term) in terms.iter().enumerate() {
        if matches!(
            term.as_str(),
            "without" | "excluding" | "except" | "avoid" | "skip" | "not" | "no"
        ) {
            for candidate in terms.iter().skip(index + 1).take(3) {
                exclusions.insert(candidate.clone());
            }
        }
    }
    exclusions
}

fn marker_values(objective: &str, marker: &str) -> BTreeSet<String> {
    objective
        .split_whitespace()
        .filter_map(|token| token.strip_prefix(marker))
        .map(|value| {
            value
                .trim_matches(|character: char| {
                    !character.is_ascii_alphanumeric() && !matches!(character, '-' | '_' | '.')
                })
                .to_ascii_lowercase()
        })
        .filter(|value| !value.is_empty())
        .collect()
}

fn normalized_terms(value: &str) -> BTreeSet<String> {
    value
        .split(|character: char| !character.is_ascii_alphanumeric())
        .filter(|term| term.len() >= 2)
        .map(str::to_ascii_lowercase)
        .collect()
}

fn contains_any(value: &str, markers: &[&str]) -> bool {
    markers.iter().any(|marker| value.contains(marker))
}

fn now_unix_ms() -> i64 {
    let nanos = OffsetDateTime::now_utc().unix_timestamp_nanos();
    i64::try_from(nanos / 1_000_000).unwrap_or(if nanos.is_negative() {
        i64::MIN
    } else {
        i64::MAX
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_and_artifact_inference_are_deterministic() {
        assert_eq!(infer_task_kind("verify the release package"), "testing");
        assert_eq!(
            infer_artifact_kind(&PromptDraft::default(), "update the README documentation"),
            "document"
        );
    }

    #[test]
    fn task_markers_are_explicit_and_bounded() {
        let objective = "deploy @approve-learning:release-rule @suppress-learning:style-rule";
        assert_eq!(
            marker_values(objective, "@approve-learning:"),
            BTreeSet::from(["release-rule".to_owned()])
        );
        assert_eq!(
            marker_values(objective, "@suppress-learning:"),
            BTreeSet::from(["style-rule".to_owned()])
        );
    }

    #[test]
    fn negative_task_language_produces_explicit_exclusions() {
        let exclusions = explicit_exclusions("write release notes without publishing artifacts");
        assert!(exclusions.contains("publishing"));
        assert!(exclusions.contains("artifacts"));
    }

    #[test]
    fn capture_disabled_short_circuits_before_selection_audit() {
        let repo = tempfile::tempdir().expect("repo");
        let store = medusa_improvement::learning_review::LearningReviewStore::for_repository(
            repo.path(),
        );
        store
            .update_privacy(
                medusa_improvement::learning_review::LearningPrivacy {
                    capture_enabled: false,
                    user_persistence_enabled: true,
                    cross_repository_reuse_enabled: true,
                    telemetry_enabled: true,
                    automatic_proposals_enabled: true,
                },
                0,
                "test",
            )
            .expect("privacy");
        let (tx, _rx) = std::sync::mpsc::channel();
        let context = select(repo.path(), &PromptDraft::default(), None, &tx);
        assert!(context.prompt_context.is_none());
        assert!(!repo.path().join(".medusa/learning-selection-audit.jsonl").exists());
    }
}
