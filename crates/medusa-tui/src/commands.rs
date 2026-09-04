use std::path::Path;

pub use medusa_runtime::commands::{
    COMMAND_SPECS, CommandSpec, CommandSuggestion, ConfigCommand, Effort, LearningCommand,
    ModelCommand, ModelConfiguration, ReviewCommand, SlashCommand, TeamCommand,
};

#[path = "voice.rs"]
pub mod voice;

pub fn parse_slash_command(input: &str) -> Result<Option<SlashCommand>, String> {
    let trimmed = input.trim();
    if trimmed.eq_ignore_ascii_case("/settings") {
        return Ok(Some(SlashCommand::Model(ModelCommand::Show)));
    }
    if trimmed.to_ascii_lowercase().starts_with("/settings ") {
        return Err("/settings does not accept arguments".to_owned());
    }
    medusa_runtime::commands::parse_slash_command(input)
}

#[must_use]
pub fn command_suggestions(input: &str, repo: &Path) -> Vec<CommandSuggestion> {
    let mut suggestions = medusa_runtime::commands::command_suggestions(input, repo);
    let Some(prefix) = input.trim_start().strip_prefix('/') else {
        return suggestions;
    };
    if prefix.contains(char::is_whitespace) {
        return suggestions;
    }
    let prefix = prefix.to_ascii_lowercase();
    if "settings".starts_with(&prefix)
        && suggestions.len() < 6
        && !suggestions
            .iter()
            .any(|suggestion| suggestion.name == "settings")
    {
        suggestions.push(CommandSuggestion {
            name: "settings".to_owned(),
            usage: "/settings".to_owned(),
            description: "edit provider profile settings".to_owned(),
        });
    }
    suggestions
}

#[must_use]
pub fn complete_first_command(input: &str, repo: &Path) -> Option<String> {
    let suggestion = command_suggestions(input, repo).into_iter().next()?;
    Some(format!("/{} ", suggestion.name))
}
