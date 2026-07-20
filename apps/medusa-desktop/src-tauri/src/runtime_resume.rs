impl RuntimeRegistry {
    fn insert_resumed(
        &self,
        repo: PathBuf,
        displayed_repo: String,
        session_id: &str,
    ) -> Result<RuntimeStartResponse, String> {
        let id = format!(
            "desktop-runtime-{}",
            self.next_id.fetch_add(1, Ordering::Relaxed) + 1
        );
        let controller = RuntimeController::start_resumed(repo.clone(), session_id)
            .map_err(|error| error.to_string())?;
        let supervisor = DaemonLaunch::for_current_executable()
            .map(|launch| DaemonSupervisor::new(&repo, launch))
            .unwrap_or_else(|_| DaemonSupervisor::observe_only(&repo));
        let entry = Arc::new(Mutex::new(RuntimeEntry {
            repo,
            controller,
            daemon: DesktopDaemon {
                supervisor,
                last_state: None,
            },
        }));
        self.entries
            .lock()
            .map_err(|_| "desktop runtime registry is poisoned".to_owned())?
            .insert(id.clone(), entry);
        Ok(RuntimeStartResponse {
            runtime_id: id,
            repo: displayed_repo,
        })
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
