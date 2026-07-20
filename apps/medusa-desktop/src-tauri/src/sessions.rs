use std::{fs, path::Path};

use medusa_agent::{SessionSummary, list_sessions};
use serde::Serialize;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopSessionSummary {
    pub id: String,
    pub objective: String,
    pub created_at: String,
    pub updated_at: String,
    pub completed: bool,
    pub waiting_for_user: bool,
    pub turn: u32,
}

impl From<SessionSummary> for DesktopSessionSummary {
    fn from(session: SessionSummary) -> Self {
        Self {
            id: session.id,
            objective: session.objective,
            created_at: session.created_at.to_string(),
            updated_at: session.updated_at.to_string(),
            completed: session.completed,
            waiting_for_user: session.waiting_for_user,
            turn: session.turn,
        }
    }
}

#[tauri::command]
pub fn runtime_list_sessions(repo: String) -> Result<Vec<DesktopSessionSummary>, String> {
    let repo = fs::canonicalize(Path::new(&repo))
        .map_err(|error| format!("cannot open {repo}: {error}"))?;
    if !repo.is_dir() {
        return Err(format!("{} is not a directory", repo.display()));
    }
    list_sessions(&repo)
        .map(|sessions| {
            sessions
                .into_iter()
                .map(DesktopSessionSummary::from)
                .collect()
        })
        .map_err(|error| error.to_string())
}
