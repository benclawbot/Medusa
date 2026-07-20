use std::{fs, process::Command};

use tempfile::tempdir;

fn medusa(config_home: &std::path::Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_medusa"));
    // Keep the spawned CLI isolated on every supported platform. Unix uses
    // XDG_CONFIG_HOME while Windows uses APPDATA.
    command
        .env("XDG_CONFIG_HOME", config_home)
        .env("APPDATA", config_home);
    command
}

#[test]
fn config_show_and_reset_cover_persisted_and_default_profiles() {
    let home = tempdir().expect("config home");
    let profile_dir = home.path().join("medusa");
    fs::create_dir_all(&profile_dir).expect("profile directory");
    let profile = profile_dir.join("provider.toml");
    fs::write(
        &profile,
        r#"connection = "omniroute"
provider = "auto/coding"
model = "auto/coding"
speed = "fast"
reasoning = "high"
auth = "existing"
base_url = "http://127.0.0.1:20128/v1"
configured = true
"#,
    )
    .expect("write profile");

    let shown = medusa(home.path())
        .args(["config", "show"])
        .output()
        .expect("show configuration");
    assert!(shown.status.success());
    let stdout = String::from_utf8(shown.stdout).expect("utf8 output");
    assert!(stdout.contains("omniroute"));
    assert!(stdout.contains("auto/coding"));
    assert!(!stdout.to_ascii_lowercase().contains("api_key"));

    let reset = medusa(home.path())
        .args(["config", "reset"])
        .output()
        .expect("reset configuration");
    assert!(reset.status.success());
    assert!(!profile.exists());
    assert!(
        String::from_utf8(reset.stdout)
            .expect("utf8 reset")
            .contains("configuration reset")
    );

    let default = medusa(home.path())
        .args(["config", "show"])
        .output()
        .expect("show defaults");
    assert!(default.status.success());
    let stdout = String::from_utf8(default.stdout).expect("utf8 defaults");
    assert!(stdout.contains("MiniMax-M3"));
    assert!(stdout.contains("configured = false"));

    let second_reset = medusa(home.path())
        .args(["config", "reset"])
        .output()
        .expect("idempotent reset");
    assert!(second_reset.status.success());
}

#[test]
fn config_show_reports_malformed_profiles_and_cli_conflicts() {
    let home = tempdir().expect("config home");
    let profile_dir = home.path().join("medusa");
    fs::create_dir_all(&profile_dir).expect("profile directory");
    fs::write(profile_dir.join("provider.toml"), "unknown = true\n")
        .expect("write malformed profile");

    let malformed = medusa(home.path())
        .args(["config", "show"])
        .output()
        .expect("show malformed configuration");
    assert!(!malformed.status.success());
    let stderr = String::from_utf8(malformed.stderr).expect("utf8 error");
    assert!(stderr.contains("unknown field") || stderr.contains("parse"));

    let conflict = medusa(home.path())
        .args(["--prompt", "hello", "config", "show"])
        .output()
        .expect("run conflicting arguments");
    assert!(!conflict.status.success());
    assert!(
        String::from_utf8(conflict.stderr)
            .expect("utf8 conflict")
            .contains("interactive-only")
    );
}

#[test]
fn config_initializes_a_non_secret_default_profile() {
    let home = tempdir().expect("config home");
    let profile = home.path().join("medusa").join("provider.toml");

    // Child-process stdin is non-interactive, so setup accepts the documented
    // defaults. This exercises the same persistence path used by the first
    // interactive configuration run without ever supplying a credential.
    let configured = medusa(home.path())
        .arg("config")
        .output()
        .expect("run first configuration");
    assert!(configured.status.success());
    let stdout = String::from_utf8(configured.stdout).expect("utf8 output");
    assert!(stdout.contains("Configuration saved to"));
    assert!(stdout.contains("API keys are not written"));

    let stored = fs::read_to_string(&profile).expect("saved provider profile");
    assert!(stored.contains("connection = \"direct\""));
    assert!(stored.contains("provider = \"minimax\""));
    assert!(stored.contains("configured = true"));
    assert!(!stored.to_ascii_lowercase().contains("api_key"));

    let shown = medusa(home.path())
        .args(["config", "show"])
        .output()
        .expect("show configured profile");
    assert!(shown.status.success());
    assert!(
        String::from_utf8(shown.stdout)
            .expect("utf8 profile")
            .contains("configured = true")
    );
}
