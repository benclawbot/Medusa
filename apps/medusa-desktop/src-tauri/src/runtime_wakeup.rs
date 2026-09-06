use std::{
    collections::BTreeSet,
    sync::{Mutex, OnceLock, TryLockError},
    thread,
    time::Duration,
};

use tauri::{AppHandle, Emitter, State};

const ACTIVE_WAKE_INTERVAL: Duration = Duration::from_millis(180);
const IDLE_WAKE_INTERVAL: Duration = Duration::from_millis(750);
const RUNTIME_WAKE_EVENT: &str = "medusa-runtime-wakeup";

static ACTIVE_WATCHERS: OnceLock<Mutex<BTreeSet<String>>> = OnceLock::new();

fn active_watchers() -> &'static Mutex<BTreeSet<String>> {
    ACTIVE_WATCHERS.get_or_init(|| Mutex::new(BTreeSet::new()))
}

/// Starts at most one presentation watcher per desktop runtime.
///
/// The watcher advances the daemon replay cursor in the backend and emits a small wake signal
/// only when new durable replay data arrives. The renderer then drains the existing canonical
/// presentation through `runtime_poll`; a low-frequency renderer fallback remains the safety net.
#[tauri::command]
pub fn runtime_begin_wakeups(
    runtime_id: String,
    app: AppHandle,
    registry: State<'_, RuntimeRegistry>,
) -> Result<(), String> {
    {
        let entries = registry
            .entries
            .lock()
            .map_err(|_| "desktop runtime registry is poisoned".to_owned())?;
        if !entries.contains_key(&runtime_id) {
            return Err(format!("runtime {runtime_id} does not exist"));
        }
    }

    {
        let mut watchers = active_watchers()
            .lock()
            .map_err(|_| "desktop runtime wakeup registry is poisoned".to_owned())?;
        if !watchers.insert(runtime_id.clone()) {
            return Ok(());
        }
    }

    let registry = registry.inner().clone();
    thread::Builder::new()
        .name(format!("medusa-desktop-wakeup-{runtime_id}"))
        .spawn(move || {
            loop {
                let entry = registry
                    .entries
                    .lock()
                    .ok()
                    .and_then(|entries| entries.get(&runtime_id).cloned());
                let Some(entry) = entry else {
                    break;
                };

                let mut active = false;
                let mut changed = false;
                match entry.try_lock() {
                    Ok(mut entry) => {
                        active = entry.session_id.is_some();
                        if active {
                            let before = entry.replay_cursor;
                            if entry.poll_daemon().is_ok() {
                                changed = entry.replay_cursor != before;
                            }
                        }
                    }
                    Err(TryLockError::WouldBlock) => {
                        // A foreground command owns the authority lock. Do not queue behind it;
                        // the next watcher pass or renderer fallback will observe the result.
                        active = true;
                    }
                    Err(TryLockError::Poisoned(_)) => break,
                }

                if changed {
                    let _ = app.emit(RUNTIME_WAKE_EVENT, &runtime_id);
                }
                thread::sleep(if active {
                    ACTIVE_WAKE_INTERVAL
                } else {
                    IDLE_WAKE_INTERVAL
                });
            }

            if let Ok(mut watchers) = active_watchers().lock() {
                watchers.remove(&runtime_id);
            }
        })
        .map_err(|error| format!("cannot start desktop runtime wakeup worker: {error}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wake_intervals_keep_active_updates_responsive_without_renderer_spin() {
        assert!(ACTIVE_WAKE_INTERVAL >= Duration::from_millis(100));
        assert!(ACTIVE_WAKE_INTERVAL < IDLE_WAKE_INTERVAL);
        assert!(IDLE_WAKE_INTERVAL >= Duration::from_millis(500));
    }
}
