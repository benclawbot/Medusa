use std::{fs, path::Path};

use medusa_agent::list_sessions;

use crate::dto::DesktopSessionSummary;

#[tauri::command]
pub fn runtime_list_sessions(repo: String) -> Result<Vec<DesktopSessionSummary>, String> {
    let repo = fs::canonicalize(Path::new(&repo))
        .map_err(|error| format!("cannot open {repo}: {error}"))?;
    if !repo.is_dir() {
        return Err(format!("{} is not a directory", repo.display()));
    }
    list_sessions(&repo)
        .map(|sessions| sessions.into_iter().map(DesktopSessionSummary::from).collect())
        .map_err(|error| error.to_string())
}
