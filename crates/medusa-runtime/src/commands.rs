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
    pub base_url: Option<String>,
}

impl std::fmt::Debug for ModelConfiguration {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ModelConfiguration")
            .field("provider", &self.provider)
            .field("model", &self.model)
            .field("effort", &self.effort)
            .field("api_key", &self.api_key.as_ref().map(|_| "<redacted>"))
            .field("base_url", &self.base_url)
            .finish()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SlashCommand {
    Help,
    Learning {
        action: LearningCommand,
    },
    Review {
        action: ReviewCommand,
    },
    Config(ConfigCommand),
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
    Team(TeamCommand),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ConfigCommand {
    Show,
    Explain,
    Profiles,
    UseProfile { name: String },
    Set { key: String, value: String },
    Unset { key: String },
    Validate,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TeamCommand {
    Show,
    Steer {
        worker_id: String,
        instruction: String,
    },
    StopWorker {
        worker_id: String,
    },
    StopTeam,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LearningCommand {
    Show {
        filter: Option<String>,
    },
    Inspect {
        id: String,
    },
    Propose {
        scope: String,
        key: String,
        value: String,
    },
    Evaluate {
        id: String,
        validation_passed: bool,
        regression_passed: bool,
        effectiveness_passed: bool,
    },
    Approve {
        id: String,
    },
    Reject {
        id: String,
    },
    Defer {
        id: String,
    },
    Validate {
        id: String,
    },
    Activate {
        id: String,
    },
    Suspend {
        id: String,
    },
    Rollback {
        id: String,
    },
    Delete {
        id: String,
    },
    Privacy,
    Export,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReviewCommand {
    Show { filter: Option<String> },
    AcceptFile { path: String },
    AcceptTask,
    RevertFile { path: String },
    RevertHunk { path: String, hunk_id: String },
    Export,
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
        name: "config",
        usage: "/config [show|explain|profiles|use <name>|set <key> <value>|unset <key>|validate]",
        description: "inspect or update shared redacted configuration",
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
        name: "team",
        usage: "/team",
        description: "show coordinated worker status",
    },
    CommandSpec {
        name: "steer",
        usage: "/steer <worker> <instruction>",
        description: "redirect a running worker between turns",
    },
    CommandSpec {
        name: "stop-worker",
        usage: "/stop-worker <worker>",
        description: "cancel one coordinated worker",
    },
    CommandSpec {
        name: "stop-team",
        usage: "/stop-team",
        description: "request graceful coordinated-team shutdown",
    },
    CommandSpec {
        name: "learning",
        usage: "/learning [show [filter]|inspect <id>|propose <scope> <key> <value>|evaluate <id> <validation> <regression> <effectiveness>|approve|reject|defer|validate|activate|suspend|rollback|delete <id>|privacy|export]",
        description: "review and control the authoritative learning lifecycle",
    },
    CommandSpec {
        name: "review",
        usage: "/review [show [filter]|accept <path>|accept-all|revert <path>|revert-hunk <path> <hunk-id>|export]",
        description: "inspect, filter, accept, revert, or export repository review state",
    },
    CommandSpec {
        name: "help",
        usage: "/help",
        description: "show available commands",
    },
];

fn parse_config_command(input: &str) -> Result<SlashCommand, String> {
    let (action, arguments) = input
        .split_once(char::is_whitespace)
        .map_or((input, ""), |(action, arguments)| {
            (action, arguments.trim())
        });
    let no_arguments = |usage: &str| {
        if arguments.is_empty() {
            Ok(())
        } else {
            Err(format!("usage: {usage}"))
        }
    };
    match action.to_ascii_lowercase().as_str() {
        "" | "show" => {
            no_arguments("/config [show]")?;
            Ok(SlashCommand::Config(ConfigCommand::Show))
        }
        "profiles" => {
            no_arguments("/config profiles")?;
            Ok(SlashCommand::Config(ConfigCommand::Profiles))
        }
        "explain" => {
            no_arguments("/config explain")?;
            Ok(SlashCommand::Config(ConfigCommand::Explain))
        }
        "validate" => {
            no_arguments("/config validate")?;
            Ok(SlashCommand::Config(ConfigCommand::Validate))
        }
        "use" => {
            if arguments.is_empty() || arguments.contains(char::is_whitespace) {
                return Err("usage: /config use <profile>".to_owned());
            }
            Ok(SlashCommand::Config(ConfigCommand::UseProfile {
                name: arguments.to_owned(),
            }))
        }
        "unset" => {
            if arguments.is_empty() || arguments.contains(char::is_whitespace) {
                return Err("usage: /config unset <key>".to_owned());
            }
            Ok(SlashCommand::Config(ConfigCommand::Unset {
                key: arguments.to_owned(),
            }))
        }
        "set" => {
            let (key, value) = arguments
                .split_once(char::is_whitespace)
                .map_or((arguments, ""), |(key, value)| (key, value.trim()));
            if key.is_empty() || value.is_empty() {
                return Err("usage: /config set <key> <value>".to_owned());
            }
            Ok(SlashCommand::Config(ConfigCommand::Set {
                key: key.to_owned(),
                value: value.to_owned(),
            }))
        }
        other => Err(format!(
            "unknown /config action `{other}`; use show, explain, profiles, use, set, unset, or validate"
        )),
    }
}

fn require_no_extra<'a>(
    parts: &mut impl Iterator<Item = &'a str>,
    usage: &str,
) -> Result<(), String> {
    if parts.next().is_none() {
        Ok(())
    } else {
        Err(format!("usage: {usage}"))
    }
}

fn parse_pass_fail(value: Option<&str>, usage: &str) -> Result<bool, String> {
    match value {
        Some("pass") | Some("passed") | Some("true") => Ok(true),
        Some("fail") | Some("failed") | Some("false") => Ok(false),
        Some(other) => Err(format!("{usage}; got {other}")),
        None => Err(format!("usage: {usage}")),
    }
}

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
        "config" => Ok(Some(parse_config_command(remainder)?)),
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
        "learning" => {
            let mut parts = remainder.split_whitespace();
            let required_id = |value: Option<&str>, action: &str| {
                value
                    .map(str::to_owned)
                    .ok_or_else(|| format!("/learning {action} expects an item id"))
            };
            let action = match parts.next() {
                None => LearningCommand::Show { filter: None },
                Some("show") => {
                    let filter = parts.next().map(str::to_owned);
                    require_no_extra(&mut parts, "/learning show [filter]")?;
                    LearningCommand::Show { filter }
                }
                Some("inspect") => {
                    let id = required_id(parts.next(), "inspect")?;
                    require_no_extra(&mut parts, "/learning inspect <id>")?;
                    LearningCommand::Inspect { id }
                }
                Some("propose") => parse_learning_propose(remainder)?,
                Some("evaluate") => {
                    let id = required_id(parts.next(), "evaluate")?;
                    let usage = "/learning evaluate <id> <validation pass|fail> <regression pass|fail> <effectiveness pass|fail>";
                    let validation_passed = parse_pass_fail(parts.next(), usage)?;
                    let regression_passed = parse_pass_fail(parts.next(), usage)?;
                    let effectiveness_passed = parse_pass_fail(parts.next(), usage)?;
                    require_no_extra(&mut parts, usage)?;
                    LearningCommand::Evaluate {
                        id,
                        validation_passed,
                        regression_passed,
                        effectiveness_passed,
                    }
                }
                Some("approve") => {
                    let id = required_id(parts.next(), "approve")?;
                    require_no_extra(&mut parts, "/learning approve <id>")?;
                    LearningCommand::Approve { id }
                }
                Some("reject") => {
                    let id = required_id(parts.next(), "reject")?;
                    require_no_extra(&mut parts, "/learning reject <id>")?;
                    LearningCommand::Reject { id }
                }
                Some("defer") => {
                    let id = required_id(parts.next(), "defer")?;
                    require_no_extra(&mut parts, "/learning defer <id>")?;
                    LearningCommand::Defer { id }
                }
                Some("validate") => {
                    let id = required_id(parts.next(), "validate")?;
                    require_no_extra(&mut parts, "/learning validate <id>")?;
                    LearningCommand::Validate { id }
                }
                Some("activate") => {
                    let id = required_id(parts.next(), "activate")?;
                    require_no_extra(&mut parts, "/learning activate <id>")?;
                    LearningCommand::Activate { id }
                }
                Some("suspend") => {
                    let id = required_id(parts.next(), "suspend")?;
                    require_no_extra(&mut parts, "/learning suspend <id>")?;
                    LearningCommand::Suspend { id }
                }
                Some("rollback") => {
                    let id = required_id(parts.next(), "rollback")?;
                    require_no_extra(&mut parts, "/learning rollback <id>")?;
                    LearningCommand::Rollback { id }
                }
                Some("delete") => {
                    let id = required_id(parts.next(), "delete")?;
                    require_no_extra(&mut parts, "/learning delete <id>")?;
                    LearningCommand::Delete { id }
                }
                Some("privacy") => {
                    require_no_extra(&mut parts, "/learning privacy")?;
                    LearningCommand::Privacy
                }
                Some("export") => {
                    require_no_extra(&mut parts, "/learning export")?;
                    LearningCommand::Export
                }
                Some(other) => return Err(format!("unknown /learning action: {other}")),
            };
            Ok(Some(SlashCommand::Learning { action }))
        }
        "review" => {
            let mut parts = remainder.split_whitespace();
            let action = match parts.next() {
                None => ReviewCommand::Show { filter: None },
                Some("show") => {
                    let filter = parts.next().map(str::to_owned);
                    require_no_extra(&mut parts, "/review show [filter]")?;
                    ReviewCommand::Show { filter }
                }
                Some("accept") => {
                    let path = parts
                        .next()
                        .ok_or_else(|| "/review accept expects a path".to_owned())?
                        .to_owned();
                    require_no_extra(&mut parts, "/review accept <path>")?;
                    ReviewCommand::AcceptFile { path }
                }
                Some("accept-all") => {
                    require_no_extra(&mut parts, "/review accept-all")?;
                    ReviewCommand::AcceptTask
                }
                Some("revert") => {
                    let path = parts
                        .next()
                        .ok_or_else(|| "/review revert expects a path".to_owned())?
                        .to_owned();
                    require_no_extra(&mut parts, "/review revert <path>")?;
                    ReviewCommand::RevertFile { path }
                }
                Some("revert-hunk") => {
                    let path = parts
                        .next()
                        .ok_or_else(|| "/review revert-hunk expects a path".to_owned())?
                        .to_owned();
                    let hunk_id = parts
                        .next()
                        .ok_or_else(|| "/review revert-hunk expects a hunk id".to_owned())?
                        .to_owned();
                    require_no_extra(&mut parts, "/review revert-hunk <path> <hunk-id>")?;
                    ReviewCommand::RevertHunk { path, hunk_id }
                }
                Some("export") => {
                    require_no_extra(&mut parts, "/review export")?;
                    ReviewCommand::Export
                }
                Some(other) => return Err(format!("unknown /review action: {other}")),
            };
            Ok(Some(SlashCommand::Review { action }))
        }
        "plan" => Ok(Some(SlashCommand::Plan {
            task: (!remainder.is_empty()).then(|| remainder.to_owned()),
        })),
        "team" => {
            require_empty("team")?;
            Ok(Some(SlashCommand::Team(TeamCommand::Show)))
        }
        "steer" => {
            let (worker_id, instruction) = remainder
                .split_once(char::is_whitespace)
                .map_or((remainder, ""), |(worker, instruction)| {
                    (worker, instruction.trim())
                });
            if worker_id.is_empty() || instruction.is_empty() {
                return Err("/steer expects <worker> <instruction>".to_owned());
            }
            Ok(Some(SlashCommand::Team(TeamCommand::Steer {
                worker_id: worker_id.to_owned(),
                instruction: instruction.to_owned(),
            })))
        }
        "stop-worker" => {
            if remainder.is_empty() || remainder.contains(char::is_whitespace) {
                return Err("/stop-worker expects exactly one worker ID".to_owned());
            }
            Ok(Some(SlashCommand::Team(TeamCommand::StopWorker {
                worker_id: remainder.to_owned(),
            })))
        }
        "stop-team" => {
            require_empty("stop-team")?;
            Ok(Some(SlashCommand::Team(TeamCommand::StopTeam)))
        }
        _ => Ok(Some(SlashCommand::Skill {
            selector: name.to_owned(),
            task: (!remainder.is_empty()).then(|| remainder.to_owned()),
        })),
    }
}

fn parse_learning_propose(remainder: &str) -> Result<LearningCommand, String> {
    let rest = remainder
        .strip_prefix("propose")
        .ok_or_else(|| "/learning propose expects scope, key, and value".to_owned())?
        .trim_start();
    let mut fields = rest.splitn(3, char::is_whitespace);
    let scope = fields
        .next()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "/learning propose expects a scope".to_owned())?;
    let key = fields
        .next()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "/learning propose expects a key".to_owned())?;
    let value = fields
        .next()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "/learning propose expects a value".to_owned())?;
    Ok(LearningCommand::Propose {
        scope: scope.to_owned(),
        key: key.to_owned(),
        value: value.to_owned(),
    })
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
        assert_eq!(
            parse_slash_command("/learning inspect proposal-1"),
            Ok(Some(SlashCommand::Learning {
                action: LearningCommand::Inspect {
                    id: "proposal-1".into()
                }
            }))
        );
        assert_eq!(
            parse_slash_command("/learning propose repository workflow collect all CI failures"),
            Ok(Some(SlashCommand::Learning {
                action: LearningCommand::Propose {
                    scope: "repository".into(),
                    key: "workflow".into(),
                    value: "collect all CI failures".into()
                }
            }))
        );
        assert_eq!(
            parse_slash_command("/learning evaluate proposal-1 pass pass fail"),
            Ok(Some(SlashCommand::Learning {
                action: LearningCommand::Evaluate {
                    id: "proposal-1".into(),
                    validation_passed: true,
                    regression_passed: true,
                    effectiveness_passed: false,
                }
            }))
        );
    }

    #[test]
    fn lifecycle_and_review_commands_reject_ambiguous_trailing_arguments() {
        for input in [
            "/learning show filter extra",
            "/learning inspect proposal-1 extra",
            "/learning evaluate proposal-1 pass pass pass extra",
            "/learning approve proposal-1 extra",
            "/learning reject proposal-1 extra",
            "/learning defer proposal-1 extra",
            "/learning validate proposal-1 extra",
            "/learning activate proposal-1 extra",
            "/learning suspend proposal-1 extra",
            "/learning rollback proposal-1 extra",
            "/learning delete proposal-1 extra",
            "/learning privacy extra",
            "/learning export extra",
            "/review show file extra",
            "/review accept file.rs extra",
            "/review accept-all extra",
            "/review revert file.rs extra",
            "/review revert-hunk file.rs h1 extra",
            "/review export extra",
        ] {
            assert!(parse_slash_command(input).is_err(), "{input}");
        }
        assert!(parse_slash_command("/learning evaluate proposal-1 pass").is_err());
        assert!(parse_slash_command("/learning evaluate proposal-1 pass maybe pass").is_err());
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
    fn parses_team_status_steering_and_cancellation_commands() {
        assert_eq!(
            parse_slash_command("/team"),
            Ok(Some(SlashCommand::Team(TeamCommand::Show)))
        );
        assert_eq!(
            parse_slash_command("/steer reviewer-1 inspect the failed assertion"),
            Ok(Some(SlashCommand::Team(TeamCommand::Steer {
                worker_id: "reviewer-1".to_owned(),
                instruction: "inspect the failed assertion".to_owned(),
            })))
        );
        assert_eq!(
            parse_slash_command("/stop-worker reviewer-1"),
            Ok(Some(SlashCommand::Team(TeamCommand::StopWorker {
                worker_id: "reviewer-1".to_owned(),
            })))
        );
        assert_eq!(
            parse_slash_command("/stop-team"),
            Ok(Some(SlashCommand::Team(TeamCommand::StopTeam)))
        );
        for input in [
            "/team extra",
            "/steer reviewer-1",
            "/stop-worker",
            "/stop-worker reviewer-1 extra",
            "/stop-team extra",
        ] {
            assert!(parse_slash_command(input).is_err(), "{input}");
        }
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
    fn config_commands_parse_and_are_discoverable() {
        assert_eq!(
            parse_slash_command("/config"),
            Ok(Some(SlashCommand::Config(ConfigCommand::Show)))
        );
        assert_eq!(
            parse_slash_command("/config profiles"),
            Ok(Some(SlashCommand::Config(ConfigCommand::Profiles)))
        );
        assert_eq!(
            parse_slash_command("/config use work"),
            Ok(Some(SlashCommand::Config(ConfigCommand::UseProfile {
                name: "work".to_owned()
            })))
        );
        assert_eq!(
            parse_slash_command("/config set model gpt-5"),
            Ok(Some(SlashCommand::Config(ConfigCommand::Set {
                key: "model".to_owned(),
                value: "gpt-5".to_owned()
            })))
        );
        assert_eq!(
            parse_slash_command("/config unset base_url"),
            Ok(Some(SlashCommand::Config(ConfigCommand::Unset {
                key: "base_url".to_owned()
            })))
        );
        assert_eq!(
            parse_slash_command("/config validate"),
            Ok(Some(SlashCommand::Config(ConfigCommand::Validate)))
        );
        assert_eq!(
            parse_slash_command("/config explain"),
            Ok(Some(SlashCommand::Config(ConfigCommand::Explain)))
        );
        assert!(parse_slash_command("/config use").is_err());
        assert!(parse_slash_command("/config set model").is_err());
        let directory = tempfile::tempdir().expect("temporary directory");
        assert_eq!(
            command_suggestions("/con", directory.path())[0].name,
            "config"
        );
        assert!(!SlashCommand::Config(ConfigCommand::Show).runs_agent());
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
            base_url: None,
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
        std::fs::write(&skill, "description: Prepare a release\nUse the checklist.")
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
