#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Effort {
    Low,
    Medium,
    High,
    Auto,
}

impl Effort {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Auto => "auto",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SlashCommand {
    Help,
    New,
    Compact { focus: Option<String> },
    Goal { objective: Option<String> },
    Model(ModelCommand),
    Effort { effort: Option<Effort> },
    Skills,
    Plan { task: Option<String> },
}

#[derive(Clone, Eq, PartialEq)]
pub enum ModelCommand {
    Show,
    SetModel(String),
    SetProvider(String),
    SetApiKey(String),
}

impl std::fmt::Debug for ModelCommand {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Show => formatter.write_str("Show"),
            Self::SetModel(model) => formatter.debug_tuple("SetModel").field(model).finish(),
            Self::SetProvider(provider) => formatter
                .debug_tuple("SetProvider")
                .field(provider)
                .finish(),
            Self::SetApiKey(_) => formatter.write_str("SetApiKey(<redacted>)"),
        }
    }
}

impl SlashCommand {
    #[must_use]
    pub fn runs_agent(&self) -> bool {
        matches!(self, Self::Plan { task: Some(_) })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CommandSpec {
    pub name: &'static str,
    pub usage: &'static str,
    pub description: &'static str,
}

pub const COMMAND_SPECS: &[CommandSpec] = &[
    CommandSpec {
        name: "new",
        usage: "/new",
        description: "start a fresh session",
    },
    CommandSpec {
        name: "compact",
        usage: "/compact [focus]",
        description: "summarize and reduce context",
    },
    CommandSpec {
        name: "goal",
        usage: "/goal [objective]",
        description: "show or set the session goal",
    },
    CommandSpec {
        name: "model",
        usage: "/model [name|provider|key]",
        description: "configure provider, model, and session key",
    },
    CommandSpec {
        name: "effort",
        usage: "/effort [low|medium|high|auto]",
        description: "show or set the turn budget",
    },
    CommandSpec {
        name: "skills",
        usage: "/skills",
        description: "list available project and user skills",
    },
    CommandSpec {
        name: "plan",
        usage: "/plan [task|off]",
        description: "enter read-only planning mode",
    },
    CommandSpec {
        name: "help",
        usage: "/help",
        description: "show available commands",
    },
];

pub fn parse_slash_command(input: &str) -> Result<Option<SlashCommand>, String> {
    let trimmed = input.trim();
    if !trimmed.starts_with('/') {
        return Ok(None);
    }
    if trimmed.contains('\n') {
        return Err("slash commands must be entered on one line".to_owned());
    }

    let body = trimmed.trim_start_matches('/');
    let (name, remainder) = body
        .split_once(char::is_whitespace)
        .map_or((body, ""), |(name, remainder)| (name, remainder.trim()));
    let require_empty = |command: &str| {
        if remainder.is_empty() {
            Ok(())
        } else {
            Err(format!("/{command} does not accept arguments"))
        }
    };

    match name.to_ascii_lowercase().as_str() {
        "help" => {
            require_empty("help")?;
            Ok(Some(SlashCommand::Help))
        }
        "new" | "clear" => {
            require_empty("new")?;
            Ok(Some(SlashCommand::New))
        }
        "compact" => Ok(Some(SlashCommand::Compact {
            focus: (!remainder.is_empty()).then(|| remainder.to_owned()),
        })),
        "goal" => Ok(Some(SlashCommand::Goal {
            objective: (!remainder.is_empty()).then(|| remainder.to_owned()),
        })),
        "model" => {
            let model_command = if remainder.is_empty() {
                ModelCommand::Show
            } else if let Some(provider) = remainder.strip_prefix("provider ") {
                let provider = provider.trim();
                if provider.is_empty() {
                    return Err("/model provider expects a provider name".to_owned());
                }
                ModelCommand::SetProvider(provider.to_ascii_lowercase())
            } else if let Some(key) = remainder
                .strip_prefix("key ")
                .or_else(|| remainder.strip_prefix("api-key "))
            {
                let key = key.trim();
                if key.is_empty() {
                    return Err("/model key expects an API key".to_owned());
                }
                ModelCommand::SetApiKey(key.to_owned())
            } else if let Some(model) = remainder.strip_prefix("model ") {
                let model = model.trim();
                if model.is_empty() {
                    return Err("/model model expects a model name".to_owned());
                }
                ModelCommand::SetModel(model.to_owned())
            } else {
                ModelCommand::SetModel(remainder.to_owned())
            };
            Ok(Some(SlashCommand::Model(model_command)))
        }
        "effort" => {
            let effort = if remainder.is_empty() {
                None
            } else {
                Some(match remainder.to_ascii_lowercase().as_str() {
                    "low" => Effort::Low,
                    "medium" => Effort::Medium,
                    "high" => Effort::High,
                    "auto" => Effort::Auto,
                    _ => {
                        return Err("/effort expects low, medium, high, or auto".to_owned());
                    }
                })
            };
            Ok(Some(SlashCommand::Effort { effort }))
        }
        "skills" => {
            require_empty("skills")?;
            Ok(Some(SlashCommand::Skills))
        }
        "plan" => Ok(Some(SlashCommand::Plan {
            task: (!remainder.is_empty()).then(|| remainder.to_owned()),
        })),
        _ => Err(format!(
            "unknown command: /{name}. Type /help for commands."
        )),
    }
}

#[must_use]
pub fn command_suggestions(input: &str) -> Vec<CommandSpec> {
    let prefix = input.trim_start().strip_prefix('/').unwrap_or_default();
    if prefix.contains(char::is_whitespace) {
        return Vec::new();
    }
    COMMAND_SPECS
        .iter()
        .copied()
        .filter(|spec| spec.name.starts_with(&prefix.to_ascii_lowercase()))
        .take(6)
        .collect()
}

#[must_use]
pub fn complete_first_command(input: &str) -> Option<String> {
    let suggestion = command_suggestions(input).into_iter().next()?;
    Some(format!("/{} ", suggestion.name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_requested_commands() {
        assert_eq!(parse_slash_command("/new"), Ok(Some(SlashCommand::New)));
        assert_eq!(
            parse_slash_command("/goal fix the login flow"),
            Ok(Some(SlashCommand::Goal {
                objective: Some("fix the login flow".to_owned())
            }))
        );
        assert_eq!(
            parse_slash_command("/effort high"),
            Ok(Some(SlashCommand::Effort {
                effort: Some(Effort::High)
            }))
        );
    }

    #[test]
    fn parses_model_configuration_without_exposing_key_text_in_debug_output() {
        assert_eq!(
            parse_slash_command("/model provider anthropic"),
            Ok(Some(SlashCommand::Model(ModelCommand::SetProvider(
                "anthropic".to_owned()
            ))))
        );
        let command = parse_slash_command("/model key secret-value").expect("parse key");
        assert!(!format!("{command:?}").contains("secret-value"));
    }

    #[test]
    fn reports_invalid_and_unknown_commands() {
        assert!(parse_slash_command("/effort extreme").is_err());
        assert!(parse_slash_command("/mystery").is_err());
        assert_eq!(parse_slash_command("fix tests"), Ok(None));
    }

    #[test]
    fn suggestions_and_tab_completion_are_prefix_aware() {
        assert_eq!(command_suggestions("/pl")[0].name, "plan");
        assert_eq!(complete_first_command("/mo"), Some("/model ".to_owned()));
        assert!(command_suggestions("/plan task").is_empty());
    }
}
