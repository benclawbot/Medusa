use std::path::PathBuf;

use medusa_daemon::{DaemonClient, JobRecord, Request, Response};

use crate::app::{AppState, TranscriptEntry};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DaemonConnectionKind {
    Connected,
    Unexpected,
    Disconnected,
}

pub(crate) type DaemonSnapshot = (Vec<JobRecord>, String);

pub(crate) struct DaemonMonitor {
    client: DaemonClient,
    last_kind: Option<DaemonConnectionKind>,
}

impl DaemonMonitor {
    pub fn new(endpoint: PathBuf) -> Self {
        Self {
            client: DaemonClient::new(endpoint),
            last_kind: None,
        }
    }

    pub fn poll(&mut self, app: &mut AppState) -> DaemonSnapshot {
        let (kind, snapshot, transition) = match self.client.request(Request::List) {
            Ok(Response::Jobs { jobs }) => {
                let transition = format!(
                    "daemon connected · {} background job{}",
                    jobs.len(),
                    if jobs.len() == 1 { "" } else { "s" }
                );
                (
                    DaemonConnectionKind::Connected,
                    (jobs, "connected".to_owned()),
                    transition,
                )
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
                let details = format!("disconnected: {error}");
                (
                    DaemonConnectionKind::Disconnected,
                    (Vec::new(), details.clone()),
                    format!("daemon {details}"),
                )
            }
        };

        if self.last_kind != Some(kind) {
            app.transcript.push(TranscriptEntry::System(transition));
            self.last_kind = Some(kind);
        }
        snapshot
    }
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

        assert!(first.1.starts_with("disconnected:"));
        assert!(second.1.starts_with("disconnected:"));
        assert_eq!(app.transcript.len(), 1);
        assert!(matches!(
            app.transcript.first(),
            Some(TranscriptEntry::System(message)) if message.starts_with("daemon disconnected:")
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
}
