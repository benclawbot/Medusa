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

#[derive(Clone, Eq, PartialEq)]
pub struct ModelConfiguration {
    pub provider: String,
    pub model: String,
    pub effort: Effort,
    pub api_key: Option<String>,
}

impl std::fmt::Debug for ModelConfiguration {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ModelConfiguration")
            .field("provider", &self.provider)
            .field("model", &self.model)
            .field("effort", &self.effort)
            .field("api_key", &self.api_key.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SlashCommand {
    Help,
    New,
    Compact {
        focus: Option<String>,
    },
    Goal {
        objective: Option<String>,
    },
    Model(ModelCommand),
    Effort {
        effort: Option<Effort>,
    },
    Skills,
    Skill {
        selector: String,
        task: Option<String>,
    },
    Plan {
        task: Option<String>,
    },
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
        matches!(
            self,
            Self::Plan { task: Some(_) } | Self::Skill { task: Some(_), .. }
        )
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
        usage: "/skills [name]",
        description: "list skills or load one by name",
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
    if trimmed == "/" {
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
                    _ => return Err("/effort expects low, medium, high, or auto".to_owned()),
                })
            };
            Ok(Some(SlashCommand::Effort { effort }))
        }
        "skills" => {
            if remainder.is_empty() {
                Ok(Some(SlashCommand::Skills))
            } else {
                let (selector, task) = remainder
                    .split_once(char::is_whitespace)
                    .map_or((remainder, ""), |(selector, task)| (selector, task.trim()));
                Ok(Some(SlashCommand::Skill {
                    selector: selector.to_owned(),
                    task: (!task.is_empty()).then(|| task.to_owned()),
                }))
            }
        }
        "plan" => Ok(Some(SlashCommand::Plan {
            task: (!remainder.is_empty()).then(|| remainder.to_owned()),
        })),
        _ => Ok(Some(SlashCommand::Skill {
            selector: name.to_owned(),
            task: (!remainder.is_empty()).then(|| remainder.to_owned()),
        })),
    }
}

#[must_use]
pub fn command_suggestions(input: &str) -> Vec<CommandSpec> {
    let Some(prefix) = input.trim_start().strip_prefix('/') else {
        return Vec::new();
    };
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
        assert_eq!(parse_slash_command("/"), Ok(None));
        assert!(parse_slash_command("/effort extreme").is_err());
        assert_eq!(
            parse_slash_command("/mystery"),
            Ok(Some(SlashCommand::Skill {
                selector: "mystery".to_owned(),
                task: None,
            }))
        );
        assert_eq!(parse_slash_command("fix tests"), Ok(None));
    }

    #[test]
    fn suggestions_and_tab_completion_are_prefix_aware() {
        assert!(command_suggestions("").is_empty());
        assert!(command_suggestions("fix tests").is_empty());
        assert_eq!(command_suggestions("/pl")[0].name, "plan");
        assert_eq!(complete_first_command("/mo"), Some("/model ".to_owned()));
        assert!(command_suggestions("/plan task").is_empty());
    }

    #[test]
    fn covers_all_effort_labels_and_parser_variants() {
        assert_eq!(Effort::Low.label(), "low");
        assert_eq!(Effort::Medium.label(), "medium");
        assert_eq!(Effort::High.label(), "high");
        assert_eq!(Effort::Auto.label(), "auto");
        for (input, expected) in [
            ("/effort low", Effort::Low),
            ("/effort medium", Effort::Medium),
            ("/effort auto", Effort::Auto),
        ] {
            assert_eq!(
                parse_slash_command(input),
                Ok(Some(SlashCommand::Effort {
                    effort: Some(expected)
                }))
            );
        }
        assert_eq!(
            parse_slash_command("/effort"),
            Ok(Some(SlashCommand::Effort { effort: None }))
        );
    }

    #[test]
    fn covers_remaining_command_and_model_branches() {
        assert_eq!(parse_slash_command("/help"), Ok(Some(SlashCommand::Help)));
        assert_eq!(parse_slash_command("/clear"), Ok(Some(SlashCommand::New)));
        assert_eq!(
            parse_slash_command("/compact"),
            Ok(Some(SlashCommand::Compact { focus: None }))
        );
        assert_eq!(
            parse_slash_command("/compact tests only"),
            Ok(Some(SlashCommand::Compact {
                focus: Some("tests only".to_owned())
            }))
        );
        assert_eq!(
            parse_slash_command("/goal"),
            Ok(Some(SlashCommand::Goal { objective: None }))
        );
        assert_eq!(
            parse_slash_command("/model"),
            Ok(Some(SlashCommand::Model(ModelCommand::Show)))
        );
        assert_eq!(
            parse_slash_command("/model model MiniMax-M3"),
            Ok(Some(SlashCommand::Model(ModelCommand::SetModel(
                "MiniMax-M3".to_owned()
            ))))
        );
        assert_eq!(
            parse_slash_command("/model direct-model"),
            Ok(Some(SlashCommand::Model(ModelCommand::SetModel(
                "direct-model".to_owned()
            ))))
        );
        assert!(matches!(
            parse_slash_command("/model api-key secret"),
            Ok(Some(SlashCommand::Model(ModelCommand::SetApiKey(_))))
        ));
        assert_eq!(
            parse_slash_command("/skills"),
            Ok(Some(SlashCommand::Skills))
        );
        assert_eq!(
            parse_slash_command("/skills release"),
            Ok(Some(SlashCommand::Skill {
                selector: "release".to_owned(),
                task: None,
            }))
        );
        assert_eq!(
            parse_slash_command("/release prepare version 1.0"),
            Ok(Some(SlashCommand::Skill {
                selector: "release".to_owned(),
                task: Some("prepare version 1.0".to_owned()),
            }))
        );
        assert_eq!(
            parse_slash_command("/plan"),
            Ok(Some(SlashCommand::Plan { task: None }))
        );
        assert_eq!(
            parse_slash_command("/plan inspect runtime"),
            Ok(Some(SlashCommand::Plan {
                task: Some("inspect runtime".to_owned())
            }))
        );
    }

    #[test]
    fn covers_validation_redaction_and_agent_classification() {
        for input in ["/help extra", "/new extra", "/help\n/new"] {
            assert!(parse_slash_command(input).is_err(), "{input}");
        }
        for input in [
            "/model provider ",
            "/model key ",
            "/model api-key ",
            "/model model ",
        ] {
            assert!(matches!(
                parse_slash_command(input),
                Ok(Some(SlashCommand::Model(ModelCommand::SetModel(_))))
            ));
        }
        let configuration = ModelConfiguration {
            provider: "anthropic".to_owned(),
            model: "claude".to_owned(),
            effort: Effort::Medium,
            api_key: Some("secret".to_owned()),
        };
        let debug = format!("{configuration:?}");
        assert!(debug.contains("<redacted>"));
        assert!(!debug.contains("secret"));
        assert!(!SlashCommand::Help.runs_agent());
        assert!(!SlashCommand::Plan { task: None }.runs_agent());
        assert!(
            SlashCommand::Plan {
                task: Some("inspect".to_owned())
            }
            .runs_agent()
        );
        assert_eq!(format!("{:?}", ModelCommand::Show), "Show");
        assert!(format!("{:?}", ModelCommand::SetModel("m".to_owned())).contains('m'));
        assert!(format!("{:?}", ModelCommand::SetProvider("p".to_owned())).contains('p'));
    }

    #[test]
    fn completion_handles_case_whitespace_and_no_match() {
        assert_eq!(command_suggestions("   /HE")[0].name, "help");
        assert_eq!(complete_first_command("/zz"), None);
        assert_eq!(command_suggestions("/").len(), 6);
    }
}
