use std::path::{Path, PathBuf};

use medusa_daemon::{
    DaemonClient, DaemonLaunch, DaemonLifecycleState, DaemonSupervisor, JobRecord, Request,
    Response,
};

use crate::app::{AppState, TranscriptEntry};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DaemonConnectionKind {
    Connected,
    Started,
    Recovered,
    Unexpected,
    Degraded,
}

pub(crate) type DaemonSnapshot = (Vec<JobRecord>, String);

pub(crate) struct DaemonMonitor {
    supervisor: Option<DaemonSupervisor>,
    client: DaemonClient,
    last_kind: Option<DaemonConnectionKind>,
}

impl DaemonMonitor {
    pub fn new(endpoint: PathBuf) -> Self {
        if let Some(repo) = repository_for_default_endpoint(&endpoint) {
            let supervisor = DaemonLaunch::for_current_executable()
                .ok()
                .map(|launch| DaemonSupervisor::new(&repo, launch))
                .unwrap_or_else(|| DaemonSupervisor::observe_only(&repo));
            let client = supervisor.client();
            return Self {
                supervisor: Some(supervisor),
                client,
                last_kind: None,
            };
        }
        Self {
            supervisor: None,
            client: DaemonClient::new(endpoint),
            last_kind: None,
        }
    }

    pub fn poll(&mut self, app: &mut AppState) -> DaemonSnapshot {
        let lifecycle = self.supervisor.as_mut().map(DaemonSupervisor::poll);
        let (kind, snapshot, transition) = match self.client.request(Request::List) {
            Ok(Response::Jobs { jobs }) => {
                let state = lifecycle
                    .as_ref()
                    .map(|value| value.state)
                    .unwrap_or(DaemonLifecycleState::Connected);
                let kind = match state {
                    DaemonLifecycleState::Started => DaemonConnectionKind::Started,
                    DaemonLifecycleState::Recovered => DaemonConnectionKind::Recovered,
                    DaemonLifecycleState::Connected | DaemonLifecycleState::Degraded => {
                        DaemonConnectionKind::Connected
                    }
                };
                let label = match kind {
                    DaemonConnectionKind::Started => "daemon started",
                    DaemonConnectionKind::Recovered => "daemon recovered",
                    _ => "daemon connected",
                };
                let transition = format!(
                    "{label} · {} background job{}",
                    jobs.len(),
                    if jobs.len() == 1 { "" } else { "s" }
                );
                (kind, (jobs, state.as_str().to_owned()), transition)
            }
            Ok(other) => {
                let details = format!("unexpected response: {other:?}");
                (
                    DaemonConnectionKind::Unexpected,
                    (Vec::new(), details.clone()),
                    format!("daemon returned an {details}"),
                )
            }
            Err(error) => {
                let lifecycle_detail = lifecycle
                    .as_ref()
                    .filter(|value| value.state == DaemonLifecycleState::Degraded)
                    .map(|value| value.detail.as_str());
                let details = match lifecycle_detail {
                    Some(detail) => format!("degraded: {detail}; connection error: {error}"),
                    None => format!("degraded: {error}"),
                };
                (
                    DaemonConnectionKind::Degraded,
                    (Vec::new(), details.clone()),
                    format!("daemon {details}"),
                )
            }
        };

        if self.should_record(kind) {
            app.transcript.push(TranscriptEntry::System(transition));
        }
        snapshot
    }

    fn should_record(&mut self, kind: DaemonConnectionKind) -> bool {
        let suppress_connected_after_start = matches!(
            (self.last_kind, kind),
            (
                Some(DaemonConnectionKind::Started | DaemonConnectionKind::Recovered),
                DaemonConnectionKind::Connected
            )
        );
        let changed = self.last_kind != Some(kind);
        self.last_kind = Some(kind);
        changed && !suppress_connected_after_start
    }
}

fn repository_for_default_endpoint(endpoint: &Path) -> Option<PathBuf> {
    if endpoint.file_name()?.to_str()? != "medusa.sock" {
        return None;
    }
    let daemon = endpoint.parent()?;
    if daemon.file_name()?.to_str()? != "daemon" {
        return None;
    }
    let medusa = daemon.parent()?;
    if medusa.file_name()?.to_str()? != ".medusa" {
        return None;
    }
    Some(medusa.parent()?.to_path_buf())
}

#[cfg(test)]
mod tests {
    use std::{sync::Arc, thread, time::Duration};

    use medusa_daemon::{DaemonPaths, spawn};

    use super::*;
    use crate::clipboard::UnsupportedClipboard;

    fn app(repo: &std::path::Path) -> AppState {
        AppState::new(
            repo.to_path_buf(),
            "daemon-monitor",
            "",
            Arc::new(UnsupportedClipboard),
        )
        .expect("app")
    }

    fn wait_for_endpoint(path: &std::path::Path) {
        for _ in 0..200 {
            if path.exists() {
                return;
            }
            thread::sleep(Duration::from_millis(20));
        }
        panic!("daemon endpoint did not appear: {}", path.display());
    }

    #[test]
    fn disconnected_transition_is_recorded_once() {
        let directory = tempfile::tempdir().expect("tempdir");
        let mut app = app(directory.path());
        let mut monitor = DaemonMonitor::new(directory.path().join("missing.sock"));

        let first = monitor.poll(&mut app);
        let second = monitor.poll(&mut app);

        assert!(first.1.starts_with("degraded:"));
        assert!(second.1.starts_with("degraded:"));
        assert_eq!(app.transcript.len(), 1);
        assert!(matches!(
            app.transcript.first(),
            Some(TranscriptEntry::System(message)) if message.starts_with("daemon degraded:")
        ));
    }

    #[test]
    fn connected_transition_uses_the_shared_daemon_contract() {
        let directory = tempfile::tempdir().expect("tempdir");
        let paths = DaemonPaths::for_repo(directory.path());
        let (handle, server) = spawn(paths.clone()).expect("spawn daemon");
        wait_for_endpoint(&paths.socket);
        let mut app = app(directory.path());
        let mut monitor = DaemonMonitor::new(paths.socket);

        let snapshot = monitor.poll(&mut app);

        assert_eq!(snapshot.1, "connected");
        assert!(snapshot.0.is_empty());
        assert!(matches!(
            app.transcript.last(),
            Some(TranscriptEntry::System(message)) if message == "daemon connected · 0 background jobs"
        ));
        handle.shutdown();
        server.join().expect("join daemon").expect("daemon result");
    }

    #[test]
    fn custom_socket_does_not_infer_repository_ownership() {
        let endpoint = PathBuf::from("/tmp/custom.sock");
        assert!(repository_for_default_endpoint(&endpoint).is_none());
    }
}
