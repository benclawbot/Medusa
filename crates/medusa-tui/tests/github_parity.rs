use std::path::PathBuf;

use medusa_tui::commands::{SlashCommand, command_suggestions, parse_slash_command};

fn repository_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

#[test]
fn github_skill_is_discoverable_from_the_tui_command_palette() {
    let suggestions = command_suggestions("/git", &repository_root());
    let github = suggestions
        .iter()
        .find(|suggestion| suggestion.name == "github")
        .expect("github parity command should be discoverable");

    assert_eq!(github.usage, "/github");
    assert!(github.description.contains("typed GitHub capability"));
}

#[test]
fn github_command_preserves_the_requested_operation_for_runtime_execution() {
    let command = parse_slash_command("/github inspect checks for HEAD")
        .expect("command should parse")
        .expect("command should be present");

    assert_eq!(
        command,
        SlashCommand::Skill {
            selector: "github".to_owned(),
            task: Some("inspect checks for HEAD".to_owned()),
        }
    );
    assert!(command.runs_agent());
}

#[test]
fn bare_github_command_loads_the_shared_workflow_without_mutating_state() {
    let command = parse_slash_command("/github")
        .expect("command should parse")
        .expect("command should be present");

    assert_eq!(
        command,
        SlashCommand::Skill {
            selector: "github".to_owned(),
            task: None,
        }
    );
    assert!(!command.runs_agent());
}
