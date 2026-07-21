use std::{fs, path::PathBuf};

use medusa_core::MedusaResult;
use serde::Serialize;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

use super::AgentSession;

const ACTIVE_SKILLS_ROOT: &str = ".medusa/skills";
const OUTCOME_ROOT: &str = ".medusa/learning/skill-outcomes";
const MAX_AUTOMATIC_SKILLS: usize = 8;

#[derive(Debug, Serialize)]
struct SkillOutcomeRecord {
    schema_version: u8,
    session_id: String,
    objective: String,
    recorded_at: String,
    completed: bool,
    verified: bool,
    turns: u32,
    evidence_count: usize,
    automatically_loaded_skills: Vec<String>,
}

pub(super) fn record_completed_session(session: &AgentSession) -> MedusaResult<Option<PathBuf>> {
    if !session.completed {
        return Ok(None);
    }

    let skills = approved_skill_names(session);
    if skills.is_empty() {
        return Ok(None);
    }

    let root = session.repo.join(OUTCOME_ROOT);
    fs::create_dir_all(&root)?;
    let destination = root.join(format!("{}.json", session.id));
    if destination.is_file() {
        return Ok(Some(destination));
    }

    let record = SkillOutcomeRecord {
        schema_version: 1,
        session_id: session.id.to_string(),
        objective: session.objective.clone(),
        recorded_at: OffsetDateTime::now_utc()
            .format(&Rfc3339)
            .map_err(medusa_core::MedusaError::from)?,
        completed: true,
        verified: !session.evidence.is_empty(),
        turns: session.turn,
        evidence_count: session.evidence.len(),
        automatically_loaded_skills: skills,
    };
    atomic_json(&destination, &record)?;
    Ok(Some(destination))
}

fn approved_skill_names(session: &AgentSession) -> Vec<String> {
    let root = session.repo.join(ACTIVE_SKILLS_ROOT);
    let mut skills = fs::read_dir(root)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            entry.path().join("SKILL.md").is_file().then_some(name)
        })
        .collect::<Vec<_>>();
    skills.sort();
    skills.dedup();
    skills.truncate(MAX_AUTOMATIC_SKILLS);
    skills
}

fn atomic_json(path: &PathBuf, value: &impl Serialize) -> MedusaResult<()> {
    let temporary = path.with_extension("json.tmp");
    fs::write(&temporary, serde_json::to_vec_pretty(value)?)?;
    fs::rename(temporary, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use medusa_core::SessionId;
    use time::OffsetDateTime;

    use super::*;

    fn session(repo: PathBuf, completed: bool) -> AgentSession {
        AgentSession {
            id: SessionId::new(),
            objective: "verify the release".to_owned(),
            repo,
            created_at: OffsetDateTime::now_utc(),
            updated_at: OffsetDateTime::now_utc(),
            completed,
            turn: 3,
            plan: Vec::new(),
            pending_question: None,
            messages: Vec::new(),
            events: Vec::new(),
            evidence: vec!["cargo test passed".to_owned()],
            tool_artifacts: Vec::new(),
        }
    }

    #[test]
    fn completed_session_records_loaded_skills_once() {
        let directory = tempfile::tempdir().expect("temporary directory");
        for name in ["release", "verify"] {
            let skill = directory
                .path()
                .join(ACTIVE_SKILLS_ROOT)
                .join(name)
                .join("SKILL.md");
            fs::create_dir_all(skill.parent().expect("skill parent")).expect("create skill");
            fs::write(skill, "# Approved skill\n").expect("write skill");
        }
        let session = session(directory.path().to_path_buf(), true);

        let first = record_completed_session(&session)
            .expect("record outcome")
            .expect("outcome path");
        let second = record_completed_session(&session)
            .expect("record outcome again")
            .expect("outcome path");

        assert_eq!(first, second);
        let value: serde_json::Value =
            serde_json::from_slice(&fs::read(first).expect("read outcome")).expect("outcome json");
        assert_eq!(value["completed"], true);
        assert_eq!(value["verified"], true);
        assert_eq!(
            value["automatically_loaded_skills"],
            serde_json::json!(["release", "verify"])
        );
    }

    #[test]
    fn incomplete_session_does_not_record_an_outcome() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let session = session(directory.path().to_path_buf(), false);
        assert_eq!(record_completed_session(&session).expect("record"), None);
    }
}
