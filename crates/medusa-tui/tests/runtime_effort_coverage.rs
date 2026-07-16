use std::{fs, thread, time::Duration};

use medusa_tui::runtime::{RuntimeController, RuntimeEvent};
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

#[test]
fn configured_turn_budgets_cover_low_and_medium_effort_bands() {
    assert_eq!(configured_effort(64), "effort:low");
    assert_eq!(configured_effort(200), "effort:medium");
}
