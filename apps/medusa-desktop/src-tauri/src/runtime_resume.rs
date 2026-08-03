impl RuntimeRegistry {
    fn insert_resumed(
        &self,
        repo: PathBuf,
        displayed_repo: String,
        session_id: &str,
    ) -> Result<RuntimeStartResponse, String> {
        let response = self.insert(repo, displayed_repo)?;
        self.with_entry(&response.runtime_id, |entry| entry.resume(session_id.to_owned()))?;
        Ok(response)
    }
}

#[tauri::command]
pub fn runtime_resume(
    repo: String,
    session_id: String,
    registry: State<'_, RuntimeRegistry>,
) -> Result<RuntimeStartResponse, String> {
    let runtime_repo = canonical_directory(Path::new(&repo))?;
    let displayed_repo = runtime_repo.to_string_lossy().into_owned();
    registry.insert_resumed(runtime_repo, displayed_repo, &session_id)
}
