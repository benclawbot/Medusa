use std::process::{Command, Output};

use tempfile::tempdir;

fn medusa(config_home: &std::path::Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_medusa"));
    command
        .env("XDG_CONFIG_HOME", config_home)
        .env("APPDATA", config_home);
    command
}

fn run(config_home: &std::path::Path, args: &[&str]) -> Output {
    medusa(config_home)
        .args(args)
        .output()
        .unwrap_or_else(|error| panic!("run {args:?}: {error}"))
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn provider_profile_cli_covers_mutation_history_and_named_profiles() {
    let home = tempdir().expect("config home");

    assert_success(&run(home.path(), &["config"]));

    let set = run(home.path(), &["config", "set", "reasoning", "high"]);
    assert_success(&set);
    assert!(String::from_utf8_lossy(&set.stdout).contains("Updated `reasoning`"));

    let history_json = run(home.path(), &["config", "history", "--json"]);
    assert_success(&history_json);
    assert!(String::from_utf8_lossy(&history_json.stdout).contains("revision"));

    let unset = run(home.path(), &["config", "unset", "reasoning"]);
    assert_success(&unset);
    assert!(String::from_utf8_lossy(&unset.stdout).contains("Reset `reasoning`"));

    let rollback = run(home.path(), &["config", "rollback"]);
    assert_success(&rollback);
    assert!(String::from_utf8_lossy(&rollback.stdout).contains("Restored previous"));

    let reset = run(home.path(), &["config", "reset-section", "preferences"]);
    assert_success(&reset);
    assert!(String::from_utf8_lossy(&reset.stdout).contains("Reset section"));

    let no_op_reset = run(home.path(), &["config", "reset-section", "preferences"]);
    assert_success(&no_op_reset);
    assert!(String::from_utf8_lossy(&no_op_reset.stdout).contains("already at defaults"));

    let invalid_section = run(home.path(), &["config", "reset-section", "invalid"]);
    assert!(!invalid_section.status.success());
    assert!(
        String::from_utf8_lossy(&invalid_section.stderr).contains("connection` or `preferences")
    );

    let created = run(home.path(), &["config", "profiles", "create", "work"]);
    assert_success(&created);
    assert!(String::from_utf8_lossy(&created.stdout).contains("Created provider profile `work`"));

    let list = run(home.path(), &["config", "profiles", "list"]);
    assert_success(&list);
    assert!(String::from_utf8_lossy(&list.stdout).contains("work"));

    let list_json = run(home.path(), &["config", "profiles", "list", "--json"]);
    assert_success(&list_json);
    assert!(String::from_utf8_lossy(&list_json.stdout).contains("\"name\": \"work\""));

    let selected = run(home.path(), &["config", "profiles", "use", "work"]);
    assert_success(&selected);
    assert!(
        String::from_utf8_lossy(&selected.stdout).contains("Active provider profile is now `work`")
    );

    let active_delete = run(home.path(), &["config", "profiles", "delete", "work"]);
    assert!(!active_delete.status.success());

    assert_success(&run(home.path(), &["config", "profiles", "use", "default"]));
    let deleted = run(home.path(), &["config", "profiles", "delete", "work"]);
    assert_success(&deleted);
    assert!(String::from_utf8_lossy(&deleted.stdout).contains("Deleted provider profile `work`"));

    let history = run(home.path(), &["config", "history"]);
    assert_success(&history);
    assert!(String::from_utf8_lossy(&history.stdout).contains("revision"));
}
