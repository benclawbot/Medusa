use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender, TrySendError},
    },
    thread::{self, JoinHandle},
};

use medusa_daemon::{DaemonClient, JobRecord, Request, Response};

use crate::app::{AppState, TranscriptEntry};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DaemonConnectionKind {
    Connected,
    Unexpected,
    Degraded,
}

pub(crate) type DaemonSnapshot = (Vec<JobRecord>, String);

#[derive(Clone, Debug)]
struct DaemonObservation {
    kind: DaemonConnectionKind,
    snapshot: DaemonSnapshot,
    transition: String,
}

/// Presentation-only daemon status observer.
///
/// The TUI runtime owns daemon startup and recovery. This monitor only observes that
/// authority. IPC is performed by one bounded worker and the terminal event loop reads
/// cached snapshots, so a slow daemon can never hold up keyboard, resize, or interrupt
/// handling. Refresh requests coalesce instead of creating another lifecycle supervisor.
pub(crate) struct DaemonMonitor {
    refresh_tx: Option<SyncSender<()>>,
    observation_rx: Receiver<DaemonObservation>,
    worker: Option<JoinHandle<()>>,
    shutting_down: Arc<AtomicBool>,
    last_kind: Option<DaemonConnectionKind>,
    snapshot: DaemonSnapshot,
}

impl DaemonMonitor {
    pub fn new(endpoint: PathBuf) -> Self {
        let client = DaemonClient::new(endpoint);
        Self::with_observer(move || observe_client(&client))
    }

    fn with_observer(mut observe: impl FnMut() -> DaemonObservation + Send + 'static) -> Self {
        // Capacity one deliberately coalesces refresh demand while an IPC call is in flight.
        let (refresh_tx, refresh_rx) = mpsc::sync_channel::<()>(1);
        let (observation_tx, observation_rx) = mpsc::channel::<DaemonObservation>();
        let shutting_down = Arc::new(AtomicBool::new(false));
        let worker_shutdown = Arc::clone(&shutting_down);
        let worker = thread::Builder::new()
            .name("medusa-tui-daemon-observer".to_owned())
            .spawn(move || {
                while refresh_rx.recv().is_ok() {
                    if worker_shutdown.load(Ordering::Acquire) {
                        break;
                    }
                    let observation = observe();
                    if worker_shutdown.load(Ordering::Acquire)
                        || observation_tx.send(observation).is_err()
                    {
                        break;
                    }
                }
            })
            .ok();
        let worker_started = worker.is_some();

        let mut monitor = Self {
            refresh_tx: worker_started.then_some(refresh_tx),
            observation_rx,
            worker,
            shutting_down,
            last_kind: None,
            snapshot: if worker_started {
                (Vec::new(), "checking".to_owned())
            } else {
                (
                    Vec::new(),
                    "degraded: daemon observer unavailable".to_owned(),
                )
            },
        };
        monitor.request_refresh();
        monitor
    }

    /// Drain completed observations and return the latest cached daemon snapshot.
    ///
    /// This method performs no socket I/O and never waits for the observer worker.
    pub fn poll(&mut self, app: &mut AppState) -> DaemonSnapshot {
        while let Ok(observation) = self.observation_rx.try_recv() {
            if self.should_record(observation.kind) {
                app.push_transcript(TranscriptEntry::System(observation.transition));
            }
            self.snapshot = observation.snapshot;
        }
        self.request_refresh();
        self.snapshot.clone()
    }

    fn request_refresh(&mut self) {
        let Some(refresh_tx) = self.refresh_tx.as_ref() else {
            return;
        };
        match refresh_tx.try_send(()) {
            Ok(()) | Err(TrySendError::Full(())) => {}
            Err(TrySendError::Disconnected(())) => self.refresh_tx = None,
        }
    }

    fn should_record(&mut self, kind: DaemonConnectionKind) -> bool {
        let changed = self.last_kind != Some(kind);
        self.last_kind = Some(kind);
        changed
    }
}

impl Drop for DaemonMonitor {
    fn drop(&mut self) {
        self.shutting_down.store(true, Ordering::Release);
        self.refresh_tx.take();
        if let Some(worker) = self.worker.take() {
            // Ordinary daemon requests have a bounded transport timeout. Joining here means
            // no presentation observer is left behind after the terminal session exits.
            let _ = worker.join();
        }
    }
}

fn observe_client(client: &DaemonClient) -> DaemonObservation {
    match client.request(Request::List) {
        Ok(Response::Jobs { jobs }) => DaemonObservation {
            kind: DaemonConnectionKind::Connected,
            transition: format!(
                "daemon connected · {} background job{}",
                jobs.len(),
                if jobs.len() == 1 { "" } else { "s" }
            ),
            snapshot: (jobs, "connected".to_owned()),
        },
        Ok(other) => {
            let details = format!("unexpected response: {other:?}");
            DaemonObservation {
                kind: DaemonConnectionKind::Unexpected,
                snapshot: (Vec::new(), details.clone()),
                transition: format!("daemon returned an {details}"),
            }
        }
        Err(error) => {
            let details = format!("degraded: {error}");
            DaemonObservation {
                kind: DaemonConnectionKind::Degraded,
                snapshot: (Vec::new(), details.clone()),
                transition: format!("daemon {details}"),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{Arc, Barrier},
        sync::atomic::{AtomicBool, Ordering},
        thread,
        time::{Duration, Instant},
    };

    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
    use medusa_daemon::{DaemonPaths, spawn};

    use super::*;
    use crate::{app::AppAction, clipboard::UnsupportedClipboard};

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

    fn wait_for_snapshot(
        monitor: &mut DaemonMonitor,
        app: &mut AppState,
        predicate: impl Fn(&DaemonSnapshot) -> bool,
    ) -> DaemonSnapshot {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let snapshot = monitor.poll(app);
            if predicate(&snapshot) {
                return snapshot;
            }
            assert!(Instant::now() < deadline, "daemon observation timed out");
            thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn disconnected_transition_is_recorded_once() {
        let directory = tempfile::tempdir().expect("tempdir");
        let mut app = app(directory.path());
        let mut monitor = DaemonMonitor::new(directory.path().join("missing.sock"));

        let first = wait_for_snapshot(&mut monitor, &mut app, |snapshot| {
            snapshot.1.starts_with("degraded:")
        });
        let _ = wait_for_snapshot(&mut monitor, &mut app, |snapshot| {
            snapshot.1.starts_with("degraded:")
        });

        assert!(first.1.starts_with("degraded:"));
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

        let snapshot =
            wait_for_snapshot(&mut monitor, &mut app, |snapshot| snapshot.1 == "connected");

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
    fn default_endpoint_monitor_never_starts_or_recovers_daemon() {
        let directory = tempfile::tempdir().expect("tempdir");
        let paths = DaemonPaths::for_repo(directory.path());
        let mut app = app(directory.path());
        let mut monitor = DaemonMonitor::new(paths.socket.clone());

        let snapshot = wait_for_snapshot(&mut monitor, &mut app, |snapshot| {
            snapshot.1.starts_with("degraded:")
        });

        assert!(snapshot.1.starts_with("degraded:"));
        assert!(!paths.owner.exists());
        assert!(!paths.startup.exists());
        assert!(!paths.socket.exists());
    }

    #[test]
    fn delayed_observer_cannot_block_typing_interrupt_or_resize() {
        let directory = tempfile::tempdir().expect("tempdir");
        let mut app = app(directory.path());
        let entered = Arc::new(Barrier::new(2));
        let release = Arc::new(Barrier::new(2));
        let first_observation = Arc::new(AtomicBool::new(true));
        let worker_entered = Arc::clone(&entered);
        let worker_release = Arc::clone(&release);
        let worker_first_observation = Arc::clone(&first_observation);
        let mut monitor = DaemonMonitor::with_observer(move || {
            if worker_first_observation.swap(false, Ordering::AcqRel) {
                worker_entered.wait();
                worker_release.wait();
            }
            DaemonObservation {
                kind: DaemonConnectionKind::Connected,
                snapshot: (Vec::new(), "connected".to_owned()),
                transition: "daemon connected · 0 background jobs".to_owned(),
            }
        });
        entered.wait();

        // The observer is deliberately blocked on `release`; prove the presentation path can
        // still complete the latency-sensitive operations before that worker is allowed to run.
        let (completed_tx, completed_rx) = mpsc::channel();
        let presentation = thread::spawn(move || {
            let snapshot = monitor.poll(&mut app);
            assert_eq!(snapshot.1, "checking");
            assert!(matches!(
                app.handle_event(Event::Key(KeyEvent::new(
                    KeyCode::Char('x'),
                    KeyModifiers::NONE,
                )))
                .expect("typing"),
                AppAction::Redraw
            ));
            assert!(matches!(
                app.handle_event(Event::Key(KeyEvent::new(
                    KeyCode::Char('c'),
                    KeyModifiers::CONTROL,
                )))
                .expect("interrupt"),
                AppAction::Interrupt
            ));
            let _ = app.handle_event(Event::Resize(120, 40)).expect("resize");
            completed_tx.send(()).expect("completion signal");
            (monitor, app)
        });
        completed_rx
            .recv_timeout(Duration::from_millis(250))
            .expect("presentation path waited on the daemon worker");

        release.wait();
        let (mut monitor, mut app) = presentation.join().expect("presentation thread");
        let snapshot =
            wait_for_snapshot(&mut monitor, &mut app, |snapshot| snapshot.1 == "connected");
        assert_eq!(snapshot.1, "connected");
        assert_eq!(
            app.transcript
                .iter()
                .filter(|entry| matches!(
                    entry,
                    TranscriptEntry::System(message)
                        if message == "daemon connected · 0 background jobs"
                ))
                .count(),
            1
        );
    }
}
