use std::{fs, thread, time::Duration};

use medusa_config::Config;
use medusa_daemon::{DaemonClient, DaemonPaths, Request, Response, spawn_with_config};
use medusa_tui::{
    commands::{Effort, ModelConfiguration},
    runtime::{RuntimeController, RuntimeEvent},
};
use tempfile::tempdir;

fn configured_effort(max_turns: u32) -> String {
    let directory = tempdir().expect("temporary directory");
    let medusa = directory.path().join(".medusa");
    fs::create_dir_all(&medusa).expect("create config directory");
    fs::write(
        medusa.join("config.toml"),
        format!("[agent]\nmax_turns = {max_turns}\n"),
    )
    .expect("write project config");

    let runtime = RuntimeController::start(directory.path().to_path_buf());
    for _ in 0..100 {
        match runtime.try_event() {
            Ok(Some(RuntimeEvent::Settings { effort, .. })) => return effort,
            Ok(Some(_)) | Ok(None) => thread::sleep(Duration::from_millis(10)),
            Err(error) => panic!("runtime failed before settings event: {error}"),
        }
    }
    panic!("runtime did not emit settings event");
}

fn wait_for_daemon(paths: &DaemonPaths) {
    let client = DaemonClient::new(&paths.socket);
    for _ in 0..200 {
        if matches!(client.request(Request::Ping), Ok(Response::Pong)) {
            return;
        }
        thread::sleep(Duration::from_millis(20));
    }
    panic!("daemon did not become ready: {}", paths.socket.display());
}

#[test]
fn configured_turn_budgets_cover_low_and_medium_effort_bands() {
    assert_eq!(configured_effort(64), "effort:low");
    assert_eq!(configured_effort(200), "effort:medium");
}

#[test]
fn model_effort_can_be_configured_before_first_session() {
    let directory = tempdir().expect("temporary directory");
    let paths = DaemonPaths::for_repo(directory.path());
    let (handle, server) =
        spawn_with_config(paths.clone(), Config::default()).expect("spawn daemon");
    wait_for_daemon(&paths);

    let runtime = RuntimeController::start(directory.path().to_path_buf());
    runtime
        .configure_model(ModelConfiguration {
            provider: "minimax".to_owned(),
            model: "MiniMax-M3".to_owned(),
            effort: Effort::Auto,
            api_key: None,
        })
        .expect("pre-session model configuration must not require a session id");

    drop(runtime);
    handle.shutdown();
    server.join().expect("join daemon").expect("daemon result");
}
