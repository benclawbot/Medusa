use std::{collections::BTreeSet, fs, path::PathBuf, process::Command};

use medusa_core::MedusaResult;
use medusa_improvement::provenance::{
    ProvenanceOutcome, ProvenanceSource, repository_identity, repository_revision,
};
use medusa_provider::MessageBlock;
use serde::Serialize;
use sha2::{Digest, Sha256};
use time::format_description::well_known::Rfc3339;

use super::{
    AgentSession,
    completed_learning::{authoritative_evidence, authoritative_success, provenance_graph},
};

const MAX_EVIDENCE_ITEMS: usize = 12;
const MAX_PROCEDURE_ITEMS: usize = 10;

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum LessonKind {
    Command,
    Debugging,
    RepositoryConvention,
    Verification,
    PlatformFix,
    Recovery,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct LessonProposal {
    id: String,
    source_session_id: String,
    created_at: String,
    repository_fingerprint: String,
    repository_revision: Option<String>,
    repository_identity: Option<medusa_improvement::scoped_memory::RepositoryIdentity>,
    provenance_observation_ids: Vec<String>,
    kind: LessonKind,
    title: String,
    summary: String,
    procedure: Vec<String>,
    evidence: Vec<String>,
    tools: BTreeSet<String>,
    confidence_milli: u16,
    status: &'static str,
}

pub(super) fn extract_completed_session(session: &AgentSession) -> MedusaResult<Option<PathBuf>> {
    let Some(proposal) = build_proposal(session)? else {
        return Ok(None);
    };

    let directory = session.repo.join(".medusa/learning/proposals");
    fs::create_dir_all(&directory)?;
    let path = directory.join(format!("{}.json", session.id));
    let temporary = path.with_extension("json.tmp");
    fs::write(&temporary, serde_json::to_vec_pretty(&proposal)?)?;
    fs::rename(temporary, &path)?;
    Ok(Some(path))
}

fn build_proposal(session: &AgentSession) -> MedusaResult<Option<LessonProposal>> {
    if !authoritative_success(session) {
        return Ok(None);
    }
    let authoritative_evidence = authoritative_evidence(session);
    if authoritative_evidence.is_empty() {
        return Ok(None);
    }

    let mut tools = BTreeSet::new();
    let mut successful_steps = Vec::new();
    let mut failed_steps = Vec::new();
    let graph = provenance_graph(session)?;
    let mut all_text = vec![session.objective.clone()];

    for message in &session.messages {
        for block in &message.content {
            match block {
                MessageBlock::Text { text } if safe_text(text) => {
                    all_text.push(compact(text, 300));
                }
                MessageBlock::Text { .. } => {}
                MessageBlock::ToolUse { name, .. } => {
                    tools.insert(name.clone());
                }
                MessageBlock::ToolResult {
                    tool_use_id,
                    is_error,
                    ..
                } => {
                    let description = format!("tool result {}", compact(tool_use_id, 120));
                    if *is_error {
                        failed_steps.push(description);
                    } else {
                        successful_steps.push(description);
                    }
                }
                MessageBlock::Image { .. } => {}
            }
        }
    }
    for observation in &graph.observations {
        if let Some(tool) = &observation.tool_name {
            tools.insert(tool.clone());
        }
        all_text.push(observation.summary.clone());
        match (observation.source, observation.outcome) {
            (ProvenanceSource::ToolExecution, ProvenanceOutcome::Positive) => {
                successful_steps.push(observation.summary.clone());
            }
            (ProvenanceSource::ToolExecution, ProvenanceOutcome::Negative) => {
                failed_steps.push(observation.summary.clone());
            }
            _ => {}
        }
    }

    let meaningful = session.turn >= 2
        || !successful_steps.is_empty()
        || !failed_steps.is_empty()
        || authoritative_evidence.len() >= 2;
    if !meaningful {
        return Ok(None);
    }

    let kind = classify(&all_text, &failed_steps, &tools);
    let mut procedure = successful_steps;
    procedure.extend(
        authoritative_evidence
            .iter()
            .filter(|value| safe_text(value))
            .map(|value| compact(value, 240)),
    );
    deduplicate(&mut procedure);
    procedure.truncate(MAX_PROCEDURE_ITEMS);

    let mut evidence = authoritative_evidence
        .iter()
        .filter(|value| safe_text(value))
        .map(|value| compact(value, 300))
        .collect::<Vec<_>>();
    evidence.extend(
        failed_steps
            .into_iter()
            .map(|step| format!("Rejected approach: {step}")),
    );
    deduplicate(&mut evidence);
    evidence.truncate(MAX_EVIDENCE_ITEMS);

    if procedure.is_empty() || evidence.is_empty() {
        return Ok(None);
    }

    let confidence = confidence_milli(session, procedure.len(), evidence.len());
    let timestamp = session.updated_at.format(&Rfc3339).map_err(|error| {
        medusa_core::MedusaError::new(
            medusa_core::ErrorCode::PersistenceFailed,
            medusa_core::ErrorCategory::Persistence,
            format!("cannot format lesson proposal timestamp: {error}"),
        )
    })?;
    let objective = compact(&session.objective, 120);

    Ok(Some(LessonProposal {
        id: format!("lesson-{}", session.id),
        source_session_id: session.id.to_string(),
        created_at: timestamp,
        repository_fingerprint: repository_fingerprint(&session.repo),
        repository_revision: repository_revision(&session.repo),
        repository_identity: repository_identity(&session.repo),
        provenance_observation_ids: graph
            .observations
            .iter()
            .map(|observation| observation.id.clone())
            .collect(),
        kind,
        title: format!("Reusable workflow: {objective}"),
        summary: format!(
            "Medusa completed this task with {} authoritative evidence item(s) using {} tool(s).",
            evidence.len(),
            tools.len()
        ),
        procedure,
        evidence,
        tools,
        confidence_milli: confidence,
        status: "proposed",
    }))
}

fn repository_fingerprint(repo: &std::path::Path) -> String {
    let identity = git_output(repo, &["remote", "get-url", "origin"])
        .map(|origin| normalize_origin(&origin))
        .or_else(|| {
            git_output(repo, &["rev-list", "--max-parents=0", "HEAD"]).map(|roots| {
                let mut roots = roots.lines().map(str::trim).collect::<Vec<_>>();
                roots.sort_unstable();
                format!("git-roots:{}", roots.join(","))
            })
        })
        .unwrap_or_else(|| "unresolved-repository".to_owned());
    hex::encode(Sha256::digest(identity.as_bytes()))
}

fn git_output(repo: &std::path::Path, arguments: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .args(arguments)
        .current_dir(repo)
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn normalize_origin(origin: &str) -> String {
    origin
        .trim()
        .trim_end_matches('/')
        .trim_end_matches(".git")
        .to_ascii_lowercase()
}

fn classify(text: &[String], failures: &[String], tools: &BTreeSet<String>) -> LessonKind {
    let corpus = text.join(" ").to_ascii_lowercase();
    if corpus.contains("windows") || corpus.contains("macos") || corpus.contains("linux") {
        LessonKind::PlatformFix
    } else if !failures.is_empty() {
        LessonKind::Recovery
    } else if corpus.contains("test") || corpus.contains("verify") || corpus.contains("check") {
        LessonKind::Verification
    } else if corpus.contains("convention")
        || corpus.contains("readme")
        || corpus.contains("repository")
    {
        LessonKind::RepositoryConvention
    } else if tools
        .iter()
        .any(|tool| tool.contains("shell") || tool.contains("command"))
    {
        LessonKind::Command
    } else {
        LessonKind::Debugging
    }
}

fn safe_text(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    !text.trim().is_empty()
        && ![
            "api_key",
            "apikey",
            "authorization:",
            "bearer ",
            "secret=",
            "token=",
        ]
        .iter()
        .any(|marker| lower.contains(marker))
}

fn compact(text: &str, limit: usize) -> String {
    let normalized = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.chars().count() <= limit {
        return normalized;
    }
    let mut shortened = normalized
        .chars()
        .take(limit.saturating_sub(1))
        .collect::<String>();
    shortened.push('…');
    shortened
}

fn deduplicate(values: &mut Vec<String>) {
    let mut seen = BTreeSet::new();
    values.retain(|value| seen.insert(value.clone()));
}

fn confidence_milli(session: &AgentSession, procedure: usize, evidence: usize) -> u16 {
    let score = 550_u16
        .saturating_add((session.turn.min(5) as u16) * 40)
        .saturating_add((procedure.min(5) as u16) * 35)
        .saturating_add((evidence.min(5) as u16) * 30);
    score.min(950)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use medusa_core::SessionId;
    use medusa_protocol::{Actor, EventPayload};
    use time::OffsetDateTime;

    use crate::evidence::append_event;

    use super::*;

    fn session(directory: &std::path::Path) -> AgentSession {
        let mut session = AgentSession {
            id: SessionId::new(),
            objective: "Fix Windows executable replacement and verify the package".to_owned(),
            repo: PathBuf::from(directory),
            created_at: OffsetDateTime::now_utc(),
            updated_at: OffsetDateTime::now_utc(),
            completed: true,
            turn: 3,
            plan: Vec::new(),
            pending_question: None,
            messages: Vec::new(),
            events: Vec::new(),
            evidence: vec![
                "cargo test --workspace passed".to_owned(),
                "Windows package smoke passed".to_owned(),
            ],
            tool_artifacts: Vec::new(),
            approval_grants: Vec::new(),
            approval_receipts: Vec::new(),
            rollback_receipts: Vec::new(),
            world_model: None,
        };
        append_event(
            &mut session,
            Actor::System("test".to_owned()),
            EventPayload::VerificationCompleted {
                passed: true,
                evidence: vec!["cargo test --workspace passed".to_owned()],
            },
        )
        .expect("verification");
        append_event(
            &mut session,
            Actor::Coordinator,
            EventPayload::SessionCompleted {
                report_ref: "report".to_owned(),
            },
        )
        .expect("completion");
        session
    }

    #[test]
    fn completed_verified_session_creates_reviewable_proposal() {
        let directory = tempfile::tempdir().expect("tempdir");
        let session = session(directory.path());
        let path = extract_completed_session(&session)
            .expect("extract")
            .expect("proposal");
        let value: serde_json::Value =
            serde_json::from_slice(&fs::read(path).expect("proposal file")).expect("proposal json");
        assert_eq!(value["source_session_id"], session.id.to_string());
        assert_eq!(value["kind"], "platform_fix");
        assert_eq!(value["status"], "proposed");
        assert!(
            value["procedure"]
                .as_array()
                .is_some_and(|items| !items.is_empty())
        );
    }

    #[test]
    fn parent_can_use_verification_evidence_when_legacy_evidence_is_empty() {
        let directory = tempfile::tempdir().expect("tempdir");
        let mut session = session(directory.path());
        session.evidence.clear();
        assert!(
            extract_completed_session(&session)
                .expect("extract")
                .is_some()
        );
    }

    #[test]
    fn incomplete_or_unverified_session_is_ignored() {
        let directory = tempfile::tempdir().expect("tempdir");
        let mut session = session(directory.path());
        session
            .events
            .retain(|event| !matches!(&event.payload, EventPayload::VerificationCompleted { .. }));
        assert!(
            extract_completed_session(&session)
                .expect("extract")
                .is_none()
        );
        session.completed = false;
        assert!(
            extract_completed_session(&session)
                .expect("extract")
                .is_none()
        );
    }

    #[test]
    fn secret_like_evidence_is_not_persisted() {
        let directory = tempfile::tempdir().expect("tempdir");
        let mut session = session(directory.path());
        session.evidence = vec![
            "api_key=sk-do-not-store".to_owned(),
            "cargo test passed".to_owned(),
        ];
        let path = extract_completed_session(&session)
            .expect("extract")
            .expect("proposal");
        let content = fs::read_to_string(path).expect("proposal");
        assert!(!content.contains("sk-do-not-store"));
        assert!(content.contains("cargo test passed"));
    }
}
