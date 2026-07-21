use std::{collections::BTreeSet, fs, path::PathBuf};

use medusa_core::MedusaResult;
use serde::Serialize;
use serde_json::Value;
use time::format_description::well_known::Rfc3339;

use super::AgentSession;

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
    if !session.completed || session.evidence.is_empty() {
        return Ok(None);
    }

    let mut tools = BTreeSet::new();
    let mut successful_steps = Vec::new();
    let mut failed_steps = Vec::new();
    let mut all_text = vec![session.objective.clone()];

    for message in &session.messages {
        let value = serde_json::to_value(message)?;
        collect_strings(&value, &mut all_text);
        collect_tool_observations(&value, &mut tools, &mut successful_steps, &mut failed_steps);
    }
    for event in &session.events {
        let value = serde_json::to_value(event)?;
        collect_strings(&value, &mut all_text);
        collect_tool_observations(&value, &mut tools, &mut successful_steps, &mut failed_steps);
    }

    let meaningful = session.turn >= 2
        || !successful_steps.is_empty()
        || !failed_steps.is_empty()
        || session.evidence.len() >= 2;
    if !meaningful {
        return Ok(None);
    }

    let kind = classify(&all_text, &failed_steps, &tools);
    let mut procedure = successful_steps;
    procedure.extend(
        session
            .evidence
            .iter()
            .filter(|value| safe_text(value))
            .map(|value| compact(value, 240)),
    );
    deduplicate(&mut procedure);
    procedure.truncate(MAX_PROCEDURE_ITEMS);

    let mut evidence = session
        .evidence
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
        repository_fingerprint: format!("path:{}", session.repo.to_string_lossy()),
        kind,
        title: format!("Reusable workflow: {objective}"),
        summary: format!(
            "Medusa completed this task with {} verified evidence item(s) using {} tool(s).",
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

fn collect_tool_observations(
    value: &Value,
    tools: &mut BTreeSet<String>,
    successful: &mut Vec<String>,
    failed: &mut Vec<String>,
) {
    match value {
        Value::Object(map) => {
            let tool = ["tool", "name"]
                .iter()
                .find_map(|key| map.get(*key).and_then(Value::as_str))
                .filter(|value| !value.trim().is_empty());
            if let Some(tool) = tool {
                tools.insert(tool.to_owned());
                let success = ["success", "ok"]
                    .iter()
                    .find_map(|key| map.get(*key).and_then(Value::as_bool));
                let description = ["text", "message", "command", "output"]
                    .iter()
                    .find_map(|key| map.get(*key).and_then(Value::as_str))
                    .filter(|text| safe_text(text))
                    .map(|text| compact(text, 240))
                    .unwrap_or_else(|| format!("Used {tool}"));
                match success {
                    Some(true) => successful.push(description),
                    Some(false) => failed.push(description),
                    None => {}
                }
            }
            for child in map.values() {
                collect_tool_observations(child, tools, successful, failed);
            }
        }
        Value::Array(values) => {
            for child in values {
                collect_tool_observations(child, tools, successful, failed);
            }
        }
        _ => {}
    }
}

fn collect_strings(value: &Value, output: &mut Vec<String>) {
    match value {
        Value::String(text) if safe_text(text) => output.push(compact(text, 300)),
        Value::Array(values) => values
            .iter()
            .for_each(|value| collect_strings(value, output)),
        Value::Object(map) => map
            .values()
            .for_each(|value| collect_strings(value, output)),
        _ => {}
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
    use time::OffsetDateTime;

    use super::*;

    fn session(directory: &std::path::Path) -> AgentSession {
        AgentSession {
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
        }
    }

    #[test]
    fn completed_verified_session_creates_reviewable_proposal() {
        let directory = tempfile::tempdir().expect("tempdir");
        let session = session(directory.path());
        let path = extract_completed_session(&session)
            .expect("extract")
            .expect("proposal");
        let value: Value =
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
    fn incomplete_or_unverified_session_is_ignored() {
        let directory = tempfile::tempdir().expect("tempdir");
        let mut session = session(directory.path());
        session.evidence.clear();
        assert!(
            extract_completed_session(&session)
                .expect("extract")
                .is_none()
        );
        session.evidence.push("verified".to_owned());
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
