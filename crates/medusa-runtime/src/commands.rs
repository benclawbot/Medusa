use std::{
    collections::BTreeMap,
    env, fs,
    io::Read,
    path::{Path, PathBuf},
};

const MAX_SKILL_DESCRIPTION_BYTES: u64 = 8 * 1024;

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

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommandSuggestion {
    pub name: String,
    pub usage: String,
    pub description: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct DiscoveredSkill {
    name: String,
    scope: String,
    description: Option<String>,
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
pub fn command_suggestions(input: &str, repo: &Path) -> Vec<CommandSuggestion> {
    let Some(prefix) = input.trim_start().strip_prefix('/') else {
        return Vec::new();
    };
    if let Some(skill_prefix) = prefix.strip_prefix("skills").and_then(|remainder| {
        remainder
            .starts_with(char::is_whitespace)
            .then(|| remainder.trim_start().to_ascii_lowercase())
    }) {
        return skill_command_suggestions(repo)
            .into_iter()
            .filter(|skill| skill.name.to_ascii_lowercase().starts_with(&skill_prefix))
            .collect();
    }
    if prefix.contains(char::is_whitespace) {
        return Vec::new();
    }
    let prefix = prefix.to_ascii_lowercase();
    let mut suggestions = COMMAND_SPECS
        .iter()
        .filter(|spec| spec.name.starts_with(&prefix))
        .map(|spec| CommandSuggestion {
            name: spec.name.to_owned(),
            usage: spec.usage.to_owned(),
            description: spec.description.to_owned(),
        })
        .take(6)
        .collect::<Vec<_>>();
    let remaining = 6_usize.saturating_sub(suggestions.len());
    suggestions.extend(
        skill_command_suggestions(repo)
            .into_iter()
            .filter(|spec| spec.name.to_ascii_lowercase().starts_with(&prefix))
            .take(remaining),
    );
    suggestions
}

#[must_use]
pub fn complete_first_command(input: &str, repo: &Path) -> Option<String> {
    let suggestion = command_suggestions(input, repo).into_iter().next()?;
    Some(format!("/{} ", suggestion.name))
}

fn skill_command_suggestions(repo: &Path) -> Vec<CommandSuggestion> {
    suggestions_for_discovered_skills(discover_skills(repo))
}

fn discover_skills(repo: &Path) -> Vec<DiscoveredSkill> {
    let mut skills = Vec::new();
    for (scope, root) in skill_roots(repo) {
        let Ok(canonical_root) = fs::canonicalize(&root) else {
            continue;
        };
        let Ok(entries) = fs::read_dir(&canonical_root) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !valid_skill_name(&name) {
                continue;
            }
            let skill = entry.path().join("SKILL.md");
            let Ok(canonical_skill) = fs::canonicalize(&skill) else {
                continue;
            };
            if !canonical_skill.starts_with(&canonical_root) || !canonical_skill.is_file() {
                continue;
            }
            skills.push(DiscoveredSkill {
                name,
                scope: scope.to_owned(),
                description: skill_description(&canonical_skill),
            });
        }
    }
    skills
}

fn skill_roots(repo: &Path) -> Vec<(&'static str, PathBuf)> {
    let mut roots = vec![
        ("project", repo.join(".medusa/skills")),
        ("project", repo.join(".claude/skills")),
    ];
    if let Some(home) = env::var_os("HOME")
        .or_else(|| env::var_os("USERPROFILE"))
        .map(PathBuf::from)
    {
        roots.push(("user", home.join(".medusa/skills")));
        roots.push(("user", home.join(".claude/skills")));
    }
    roots
}

fn skill_description(path: &Path) -> Option<String> {
    let mut reader = fs::File::open(path).ok()?.take(MAX_SKILL_DESCRIPTION_BYTES);
    let mut text = String::new();
    reader.read_to_string(&mut text).ok()?;
    text.lines().find_map(|line| {
        line.strip_prefix("description:")
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|value| value.trim_matches('"').to_owned())
    })
}

fn valid_skill_name(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && !name.contains('/')
        && !name.as_bytes().contains(&92)
        && !name.contains('@')
        && !name.contains("..")
}

fn suggestions_for_discovered_skills(skills: Vec<DiscoveredSkill>) -> Vec<CommandSuggestion> {
    let mut by_name = BTreeMap::<String, Vec<DiscoveredSkill>>::new();
    for skill in skills {
        by_name.entry(skill.name.clone()).or_default().push(skill);
    }
    let built_in_names = COMMAND_SPECS
        .iter()
        .map(|spec| spec.name)
        .collect::<Vec<_>>();
    let mut suggestions = Vec::new();
    for (name, named_skills) in by_name {
        let built_in_collision = built_in_names.contains(&name.as_str());
        if named_skills.len() == 1 && !built_in_collision {
            let skill = &named_skills[0];
            suggestions.push(skill_suggestion(name, skill));
            continue;
        }
        let mut by_scope = BTreeMap::<String, Vec<DiscoveredSkill>>::new();
        for skill in named_skills {
            by_scope.entry(skill.scope.clone()).or_default().push(skill);
        }
        for (scope, scoped_skills) in by_scope {
            if scoped_skills.len() != 1 {
                continue;
            }
            let selector = format!("{name}@{scope}");
            suggestions.push(skill_suggestion(selector, &scoped_skills[0]));
        }
    }
    suggestions
}

fn skill_suggestion(selector: String, skill: &DiscoveredSkill) -> CommandSuggestion {
    let description = skill.description.as_deref().map_or_else(
        || format!("installed {} skill", skill.scope),
        |description| format!("{} skill - {description}", skill.scope),
    );
    CommandSuggestion {
        usage: format!("/{selector} [task]"),
        name: selector,
        description,
    }
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
        let directory = tempfile::tempdir().expect("temporary directory");
        assert!(command_suggestions("", directory.path()).is_empty());
        assert!(command_suggestions("fix tests", directory.path()).is_empty());
        assert_eq!(command_suggestions("/pl", directory.path())[0].name, "plan");
        assert_eq!(
            complete_first_command("/mo", directory.path()),
            Some("/model ".to_owned())
        );
        assert!(command_suggestions("/plan task", directory.path()).is_empty());
    }

    #[test]
    fn skills_command_lists_installed_skills_for_selection() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let skill = directory.path().join(".medusa/skills/release");
        fs::create_dir_all(&skill).expect("create skill directory");
        fs::write(
            skill.join("SKILL.md"),
            "---\ndescription: Prepare a release\n---\n",
        )
        .expect("write skill");

        let suggestions = command_suggestions("/skills ", directory.path());
        let release = suggestions
            .iter()
            .find(|suggestion| suggestion.name == "release")
            .expect("project skill is selectable");
        assert!(release.description.contains("Prepare a release"));
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
    fn installed_skills_are_discovered_live_and_completed() {
        let directory = tempfile::tempdir().expect("temporary directory");
        assert!(command_suggestions("/rel", directory.path()).is_empty());
        let skill = directory.path().join(".medusa/skills/release/SKILL.md");
        std::fs::create_dir_all(skill.parent().expect("skill directory"))
            .expect("create skill directory");
        std::fs::write(
            &skill,
            "description: Prepare a release
Use the checklist.",
        )
        .expect("write skill");

        let suggestions = command_suggestions("/rel", directory.path());
        assert_eq!(suggestions[0].name, "release");
        assert_eq!(suggestions[0].usage, "/release [task]");
        assert!(suggestions[0].description.contains("Prepare a release"));
        assert_eq!(
            complete_first_command("/rel", directory.path()),
            Some("/release ".to_owned())
        );
    }

    #[test]
    fn invalid_skill_names_are_never_suggested() {
        for name in ["", ".", "..", "bad@name", "bad..name", "bad\\name"] {
            assert!(!valid_skill_name(name), "{name}");
        }
        assert!(valid_skill_name("release-tools"));
    }

    #[cfg(unix)]
    #[test]
    fn escaped_skill_symlink_is_not_suggested() {
        use std::os::unix::fs::symlink;

        let repository = tempfile::tempdir().expect("repository");
        let outside = tempfile::tempdir().expect("outside directory");
        let outside_skill = outside.path().join("escaped");
        std::fs::create_dir_all(&outside_skill).expect("outside skill directory");
        std::fs::write(
            outside_skill.join("SKILL.md"),
            "description: Escaped instructions",
        )
        .expect("outside skill");
        let root = repository.path().join(".medusa/skills");
        std::fs::create_dir_all(&root).expect("skill root");
        symlink(&outside_skill, root.join("escaped")).expect("skill symlink");

        assert!(command_suggestions("/esc", repository.path()).is_empty());
    }

    #[test]
    fn colliding_skills_receive_scope_suffixes_and_invalid_duplicates_are_hidden() {
        let scoped = suggestions_for_discovered_skills(vec![
            DiscoveredSkill {
                name: "release".to_owned(),
                scope: "project".to_owned(),
                description: None,
            },
            DiscoveredSkill {
                name: "release".to_owned(),
                scope: "user".to_owned(),
                description: None,
            },
            DiscoveredSkill {
                name: "plan".to_owned(),
                scope: "project".to_owned(),
                description: None,
            },
        ]);
        let names = scoped
            .iter()
            .map(|spec| spec.name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            names,
            vec!["plan@project", "release@project", "release@user"]
        );

        let ambiguous = suggestions_for_discovered_skills(vec![
            DiscoveredSkill {
                name: "release".to_owned(),
                scope: "project".to_owned(),
                description: None,
            },
            DiscoveredSkill {
                name: "release".to_owned(),
                scope: "project".to_owned(),
                description: None,
            },
        ]);
        assert!(ambiguous.is_empty());
    }

    #[test]
    fn completion_handles_case_whitespace_and_no_match() {
        let directory = tempfile::tempdir().expect("temporary directory");
        assert_eq!(
            command_suggestions("   /HE", directory.path())[0].name,
            "help"
        );
        assert_eq!(complete_first_command("/zz", directory.path()), None);
        assert_eq!(command_suggestions("/", directory.path()).len(), 6);
    }
}
