use std::io::{self, IsTerminal, Write};

use crossterm::{
    cursor::{Hide, MoveTo, Show},
    event::{self, Event, KeyCode, KeyEventKind},
    execute, queue,
    style::{Attribute, Print, SetAttribute},
    terminal::{
        Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode,
        enable_raw_mode, size,
    },
};
use medusa_config::ProviderProfile;

use crate::input::SelectionState;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExistingProfileChoice {
    pub name: String,
    pub provider: String,
    pub model: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FirstRunSetupRequest {
    pub initial_profile: ProviderProfile,
    pub existing_profiles: Vec<ExistingProfileChoice>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FirstRunSetupOutcome {
    Configure(ProviderProfile),
    UseExisting(String),
    Cancelled,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SetupMode {
    Quick,
    Advanced,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SetupStep {
    Mode,
    ExistingProfile,
    Connection,
    Model,
    Speed,
    Reasoning,
    Authentication,
    Review,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct SetupChoice {
    label: String,
    description: String,
}

struct SetupState {
    profile: ProviderProfile,
    existing_profiles: Vec<ExistingProfileChoice>,
    step: SetupStep,
    history: Vec<SetupStep>,
    mode: Option<SetupMode>,
    selected_existing: Option<usize>,
    selection: SelectionState,
}

impl SetupState {
    fn new(request: FirstRunSetupRequest) -> Self {
        Self {
            profile: request.initial_profile,
            existing_profiles: request.existing_profiles,
            step: SetupStep::Mode,
            history: Vec::new(),
            mode: None,
            selected_existing: None,
            selection: SelectionState::new(0),
        }
    }

    fn choices(&self) -> Vec<SetupChoice> {
        match self.step {
            SetupStep::Mode => {
                let mut choices = vec![
                    SetupChoice {
                        label: "Quick setup".to_owned(),
                        description: "Recommended secure defaults with only essential choices"
                            .to_owned(),
                    },
                    SetupChoice {
                        label: "Advanced setup".to_owned(),
                        description: "Choose model speed, reasoning, and authentication".to_owned(),
                    },
                ];
                if !self.existing_profiles.is_empty() {
                    choices.push(SetupChoice {
                        label: "Existing profile".to_owned(),
                        description: "Use one of your already configured named profiles".to_owned(),
                    });
                }
                choices
            }
            SetupStep::ExistingProfile => self
                .existing_profiles
                .iter()
                .map(|profile| SetupChoice {
                    label: profile.name.clone(),
                    description: format!("{} / {}", profile.provider, profile.model),
                })
                .collect(),
            SetupStep::Connection => vec![
                SetupChoice {
                    label: "OmniRoute".to_owned(),
                    description: "Managed or existing local gateway (recommended)".to_owned(),
                },
                SetupChoice {
                    label: "ChatGPT OAuth".to_owned(),
                    description: "Use the local openai-oauth gateway".to_owned(),
                },
                SetupChoice {
                    label: "OpenAI API".to_owned(),
                    description: "Use OPENAI_API_KEY with the official endpoint".to_owned(),
                },
                SetupChoice {
                    label: "MiniMax direct".to_owned(),
                    description: "Use MINIMAX_API_KEY with the native provider".to_owned(),
                },
                SetupChoice {
                    label: "Local runtime".to_owned(),
                    description: "OpenAI-compatible runtime on 127.0.0.1:11434".to_owned(),
                },
                SetupChoice {
                    label: "Custom OpenAI-compatible".to_owned(),
                    description: "Keep the current custom endpoint/model values".to_owned(),
                },
            ],
            SetupStep::Model => self
                .model_values()
                .into_iter()
                .map(|model| SetupChoice {
                    description: model_description(&self.profile.connection, &model),
                    label: model,
                })
                .collect(),
            SetupStep::Speed => [
                ("fast", "Fast"),
                ("balanced", "Balanced"),
                ("quality", "Maximum quality"),
                ("custom", "Custom runtime policy"),
            ]
            .into_iter()
            .map(|(value, label)| SetupChoice {
                label: label.to_owned(),
                description: format!("Set speed policy to {value}"),
            })
            .collect(),
            SetupStep::Reasoning => [
                ("low", "Low"),
                ("medium", "Medium"),
                ("high", "High"),
                ("maximum", "Maximum"),
            ]
            .into_iter()
            .map(|(value, label)| SetupChoice {
                label: label.to_owned(),
                description: format!("Set reasoning level to {value}"),
            })
            .collect(),
            SetupStep::Authentication => [
                ("oauth", "OAuth / browser sign-in"),
                ("api-key", "API key from environment"),
                ("existing", "Existing gateway credentials"),
                ("none", "No authentication"),
            ]
            .into_iter()
            .map(|(value, label)| SetupChoice {
                label: label.to_owned(),
                description: format!("Authentication mode: {value}"),
            })
            .collect(),
            SetupStep::Review => vec![
                SetupChoice {
                    label: if self.selected_existing.is_some() {
                        "Use profile and continue".to_owned()
                    } else {
                        "Save and continue".to_owned()
                    },
                    description: "Validate through Medusa's existing configuration authority"
                        .to_owned(),
                },
                SetupChoice {
                    label: "Back".to_owned(),
                    description: "Return to the previous setup screen".to_owned(),
                },
            ],
        }
    }

    fn title(&self) -> &'static str {
        match self.step {
            SetupStep::Mode => "Welcome to Medusa",
            SetupStep::ExistingProfile => "Choose an existing profile",
            SetupStep::Connection => "Choose a model connection",
            SetupStep::Model => "Choose a model",
            SetupStep::Speed => "Choose speed",
            SetupStep::Reasoning => "Choose reasoning",
            SetupStep::Authentication => "Choose authentication",
            SetupStep::Review => "Review first-run setup",
        }
    }

    fn subtitle(&self) -> &'static str {
        match self.step {
            SetupStep::Mode => {
                "First-run setup stays inside the terminal UI and never stores credentials in provider.toml."
            }
            SetupStep::ExistingProfile => {
                "Selecting a profile changes only the active catalog selection after validation."
            }
            SetupStep::Connection => {
                "Provider discovery and browser OAuth will expand these choices in issue #801."
            }
            SetupStep::Model => {
                "Choose the model value that will be validated before the profile is committed."
            }
            SetupStep::Speed => "This controls the existing ProviderProfile speed setting.",
            SetupStep::Reasoning => "This controls the existing ProviderProfile reasoning setting.",
            SetupStep::Authentication => {
                "Credentials remain owned by the environment, provider, or gateway."
            }
            SetupStep::Review => {
                "Repository mutation, containment, approvals, and verification remain governed by Medusa."
            }
        }
    }

    fn move_selection(&mut self, delta: isize) {
        let count = self.choices().len();
        self.selection.move_by(count, delta);
    }

    fn enter(&mut self) -> Option<FirstRunSetupOutcome> {
        let selected = self.selection.selected();
        match self.step {
            SetupStep::Mode => {
                match selected {
                    0 => {
                        self.mode = Some(SetupMode::Quick);
                        self.selected_existing = None;
                        self.go_to(SetupStep::Connection);
                        self.selection = SelectionState::new(0);
                    }
                    1 => {
                        self.mode = Some(SetupMode::Advanced);
                        self.selected_existing = None;
                        self.go_to(SetupStep::Connection);
                    }
                    2 if !self.existing_profiles.is_empty() => {
                        self.mode = None;
                        self.go_to(SetupStep::ExistingProfile);
                    }
                    _ => {}
                }
                None
            }
            SetupStep::ExistingProfile => {
                if selected < self.existing_profiles.len() {
                    self.selected_existing = Some(selected);
                    self.go_to(SetupStep::Review);
                }
                None
            }
            SetupStep::Connection => {
                self.apply_connection(selected);
                self.go_to(SetupStep::Model);
                None
            }
            SetupStep::Model => {
                if let Some(model) = self.model_values().get(selected) {
                    self.profile.model.clone_from(model);
                }
                if self.mode == Some(SetupMode::Advanced) {
                    self.go_to(SetupStep::Speed);
                } else {
                    self.go_to(SetupStep::Review);
                }
                None
            }
            SetupStep::Speed => {
                if let Some(value) = ["fast", "balanced", "quality", "custom"].get(selected) {
                    self.profile.speed = (*value).to_owned();
                }
                self.go_to(SetupStep::Reasoning);
                None
            }
            SetupStep::Reasoning => {
                if let Some(value) = ["low", "medium", "high", "maximum"].get(selected) {
                    self.profile.reasoning = (*value).to_owned();
                }
                if self.auth_is_fixed() {
                    self.go_to(SetupStep::Review);
                } else {
                    self.go_to(SetupStep::Authentication);
                }
                None
            }
            SetupStep::Authentication => {
                if let Some(value) = ["oauth", "api-key", "existing", "none"].get(selected) {
                    self.profile.auth = (*value).to_owned();
                }
                self.go_to(SetupStep::Review);
                None
            }
            SetupStep::Review => {
                if selected == 1 {
                    self.back();
                    return None;
                }
                if let Some(index) = self.selected_existing {
                    return self
                        .existing_profiles
                        .get(index)
                        .map(|profile| FirstRunSetupOutcome::UseExisting(profile.name.clone()));
                }
                let mut profile = self.profile.clone();
                profile.configured = true;
                Some(FirstRunSetupOutcome::Configure(profile))
            }
        }
    }

    fn escape(&mut self) -> Option<FirstRunSetupOutcome> {
        if self.step == SetupStep::Mode {
            return Some(FirstRunSetupOutcome::Cancelled);
        }
        self.back();
        None
    }

    fn go_to(&mut self, next: SetupStep) {
        self.history.push(self.step);
        self.step = next;
        self.reset_selection_for_step();
    }

    fn back(&mut self) {
        if let Some(previous) = self.history.pop() {
            self.step = previous;
            self.reset_selection_for_step();
        }
    }

    fn reset_selection_for_step(&mut self) {
        let selected = match self.step {
            SetupStep::Mode => match self.mode {
                Some(SetupMode::Quick) => 0,
                Some(SetupMode::Advanced) => 1,
                None if self.selected_existing.is_some() && !self.existing_profiles.is_empty() => 2,
                None => 0,
            },
            SetupStep::ExistingProfile => self.selected_existing.unwrap_or(0),
            SetupStep::Connection => connection_index(&self.profile.connection),
            SetupStep::Model => self
                .model_values()
                .iter()
                .position(|model| model == &self.profile.model)
                .unwrap_or(0),
            SetupStep::Speed => ["fast", "balanced", "quality", "custom"]
                .iter()
                .position(|value| *value == self.profile.speed)
                .unwrap_or(1),
            SetupStep::Reasoning => ["low", "medium", "high", "maximum"]
                .iter()
                .position(|value| *value == self.profile.reasoning)
                .unwrap_or(1),
            SetupStep::Authentication => ["oauth", "api-key", "existing", "none"]
                .iter()
                .position(|value| *value == self.profile.auth)
                .unwrap_or(2),
            SetupStep::Review => 0,
        };
        self.selection = SelectionState::new(selected);
    }

    fn apply_connection(&mut self, selected: usize) {
        match selected {
            0 => {
                self.profile.connection = "omniroute".to_owned();
                self.profile.provider = "auto/coding".to_owned();
                self.profile.model = "auto/coding".to_owned();
                self.profile.auth = "existing".to_owned();
                self.profile.base_url = Some("http://127.0.0.1:20128/v1".to_owned());
            }
            1 => {
                self.profile.connection = "chatgpt-oauth".to_owned();
                self.profile.provider = "openai-oauth".to_owned();
                self.profile.model = "gpt-5".to_owned();
                self.profile.auth = "none".to_owned();
                self.profile.base_url = Some("http://127.0.0.1:10531/v1".to_owned());
            }
            2 => {
                self.profile.connection = "openai-api".to_owned();
                self.profile.provider = "openai".to_owned();
                self.profile.model = "gpt-5".to_owned();
                self.profile.auth = "api-key".to_owned();
                self.profile.base_url = Some("https://api.openai.com/v1".to_owned());
            }
            3 => {
                self.profile.connection = "direct".to_owned();
                self.profile.provider = "minimax".to_owned();
                self.profile.model = "MiniMax-M3".to_owned();
                self.profile.auth = "api-key".to_owned();
                self.profile.base_url = None;
            }
            4 => {
                self.profile.connection = "local".to_owned();
                self.profile.provider = "local".to_owned();
                self.profile.model = "MiniMax-M3".to_owned();
                self.profile.auth = "none".to_owned();
                self.profile.base_url = Some("http://127.0.0.1:11434/v1".to_owned());
            }
            5 => {
                self.profile.connection = "openai-compatible".to_owned();
                if self.profile.provider.trim().is_empty() {
                    self.profile.provider = "openai-compatible".to_owned();
                }
                if self.profile.model.trim().is_empty() {
                    self.profile.model = "MiniMax-M3".to_owned();
                }
                if self.profile.base_url.is_none() {
                    self.profile.base_url = Some("http://127.0.0.1:8000/v1".to_owned());
                }
                if !matches!(
                    self.profile.auth.as_str(),
                    "oauth" | "api-key" | "existing" | "none"
                ) {
                    self.profile.auth = "existing".to_owned();
                }
            }
            _ => {}
        }
        self.profile.configured = false;
    }

    fn model_values(&self) -> Vec<String> {
        let recommended = match self.profile.connection.as_str() {
            "omniroute" => "auto/coding",
            "chatgpt-oauth" | "openai-api" => "gpt-5",
            "direct" | "local" | "openai-compatible" => "MiniMax-M3",
            _ => "MiniMax-M3",
        };
        let mut models = vec![recommended.to_owned()];
        if !self.profile.model.trim().is_empty() && self.profile.model != recommended {
            models.push(self.profile.model.clone());
        }
        models
    }

    fn auth_is_fixed(&self) -> bool {
        matches!(
            self.profile.connection.as_str(),
            "chatgpt-oauth" | "openai-api"
        )
    }

    fn review_lines(&self) -> Vec<String> {
        if let Some(index) = self.selected_existing
            && let Some(profile) = self.existing_profiles.get(index)
        {
            return vec![
                format!("Profile:   {}", profile.name),
                format!("Provider:  {}", profile.provider),
                format!("Model:     {}", profile.model),
                String::new(),
                "The existing named profile will become active only after validation succeeds."
                    .to_owned(),
            ];
        }
        let mut lines = vec![
            format!("Connection: {}", self.profile.connection),
            format!("Provider:   {}", self.profile.provider),
            format!("Model:      {}", self.profile.model),
            format!("Speed:      {}", self.profile.speed),
            format!("Reasoning:  {}", self.profile.reasoning),
            format!("Auth:       {}", self.profile.auth),
        ];
        if let Some(base_url) = self.profile.base_url.as_deref() {
            lines.push(format!("Base URL:   {base_url}"));
        }
        lines.push(String::new());
        lines.push("No credential value is stored in the provider profile.".to_owned());
        lines
    }
}

pub fn run_first_run_setup(request: FirstRunSetupRequest) -> io::Result<FirstRunSetupOutcome> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "first-run setup requires an interactive terminal",
        ));
    }

    let mut terminal = SetupTerminal::enter()?;
    let mut state = SetupState::new(request);
    loop {
        terminal.render(&state)?;
        let event = event::read()?;
        let Event::Key(key) = event else {
            continue;
        };
        if key.kind == KeyEventKind::Release {
            continue;
        }
        let outcome = match key.code {
            KeyCode::Up => {
                state.move_selection(-1);
                None
            }
            KeyCode::Down => {
                state.move_selection(1);
                None
            }
            KeyCode::Home => {
                state.selection.set_selected(0, state.choices().len());
                None
            }
            KeyCode::End => {
                let count = state.choices().len();
                state.selection.set_selected(count.saturating_sub(1), count);
                None
            }
            KeyCode::Enter => state.enter(),
            KeyCode::Esc => state.escape(),
            KeyCode::Char('c')
                if key
                    .modifiers
                    .contains(crossterm::event::KeyModifiers::CONTROL) =>
            {
                Some(FirstRunSetupOutcome::Cancelled)
            }
            _ => None,
        };
        if let Some(outcome) = outcome {
            terminal.restore();
            return Ok(outcome);
        }
    }
}

struct SetupTerminal {
    stdout: io::Stdout,
    active: bool,
}

impl SetupTerminal {
    fn enter() -> io::Result<Self> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        if let Err(error) = execute!(stdout, EnterAlternateScreen, Hide) {
            let _ = disable_raw_mode();
            return Err(error);
        }
        Ok(Self {
            stdout,
            active: true,
        })
    }

    fn render(&mut self, state: &SetupState) -> io::Result<()> {
        let (width, _) = size().unwrap_or((80, 24));
        queue!(
            self.stdout,
            MoveTo(0, 0),
            Clear(ClearType::All),
            SetAttribute(Attribute::Bold),
            Print(clip_line(state.title(), width)),
            SetAttribute(Attribute::Reset),
            Print("\r\n"),
            Print(clip_line(state.subtitle(), width)),
            Print("\r\n\r\n")
        )?;

        if state.step == SetupStep::Review {
            for line in state.review_lines() {
                queue!(self.stdout, Print(clip_line(&line, width)), Print("\r\n"))?;
            }
            queue!(self.stdout, Print("\r\n"))?;
        }

        let choices = state.choices();
        for (index, choice) in choices.iter().enumerate() {
            let marker = if index == state.selection.selected() {
                "›"
            } else {
                " "
            };
            if index == state.selection.selected() {
                queue!(self.stdout, SetAttribute(Attribute::Reverse))?;
            }
            queue!(
                self.stdout,
                Print(clip_line(&format!("{marker} {}", choice.label), width)),
                SetAttribute(Attribute::Reset),
                Print("\r\n"),
                Print(clip_line(&format!("    {}", choice.description), width)),
                Print("\r\n")
            )?;
        }

        queue!(
            self.stdout,
            Print("\r\n"),
            SetAttribute(Attribute::Dim),
            Print(clip_line(
                "↑/↓ move · Enter select · Esc back one screen · Ctrl+C cancel",
                width
            )),
            SetAttribute(Attribute::Reset)
        )?;
        self.stdout.flush()
    }

    fn restore(&mut self) {
        if !self.active {
            return;
        }
        let _ = disable_raw_mode();
        let _ = execute!(self.stdout, Show, LeaveAlternateScreen);
        self.active = false;
    }
}

impl Drop for SetupTerminal {
    fn drop(&mut self) {
        self.restore();
    }
}

fn connection_index(connection: &str) -> usize {
    match connection {
        "omniroute" => 0,
        "chatgpt-oauth" => 1,
        "openai-api" => 2,
        "direct" => 3,
        "local" => 4,
        "openai-compatible" => 5,
        _ => 0,
    }
}

fn model_description(connection: &str, model: &str) -> String {
    match connection {
        "omniroute" => "OmniRoute automatic coding route".to_owned(),
        "chatgpt-oauth" => "Model checked by the existing OAuth gateway preflight".to_owned(),
        "openai-api" => "Official OpenAI API model".to_owned(),
        "direct" => "Direct MiniMax model".to_owned(),
        "local" => format!("Local runtime model: {model}"),
        "openai-compatible" => format!("OpenAI-compatible model: {model}"),
        _ => model.to_owned(),
    }
}

fn clip_line(value: &str, width: u16) -> String {
    let width = usize::from(width.max(1));
    if value.chars().count() <= width {
        return value.to_owned();
    }
    if width <= 1 {
        return "…".to_owned();
    }
    value
        .chars()
        .take(width.saturating_sub(1))
        .chain(std::iter::once('…'))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enter(state: &mut SetupState) -> Option<FirstRunSetupOutcome> {
        state.enter()
    }

    #[test]
    fn quick_setup_reaches_a_configured_profile_without_free_text() {
        let mut state = SetupState::new(FirstRunSetupRequest {
            initial_profile: ProviderProfile::default(),
            existing_profiles: Vec::new(),
        });

        enter(&mut state);
        assert_eq!(state.step, SetupStep::Connection);
        assert_eq!(state.selection.selected(), 0);
        enter(&mut state);
        assert_eq!(state.step, SetupStep::Model);
        enter(&mut state);
        assert_eq!(state.step, SetupStep::Review);
        let outcome = enter(&mut state).expect("outcome");

        let FirstRunSetupOutcome::Configure(profile) = outcome else {
            panic!("expected configured profile");
        };
        assert!(profile.configured);
        assert_eq!(profile.connection, "omniroute");
        assert_eq!(profile.provider, "auto/coding");
        assert_eq!(profile.model, "auto/coding");
        profile.validate().expect("valid profile");
    }

    #[test]
    fn advanced_back_navigation_preserves_the_connection_choice() {
        let mut state = SetupState::new(FirstRunSetupRequest {
            initial_profile: ProviderProfile::default(),
            existing_profiles: Vec::new(),
        });
        state.move_selection(1);
        enter(&mut state);
        assert_eq!(state.mode, Some(SetupMode::Advanced));
        state.selection.set_selected(2, state.choices().len());
        enter(&mut state);
        assert_eq!(state.profile.connection, "openai-api");
        assert_eq!(state.step, SetupStep::Model);

        assert!(state.escape().is_none());
        assert_eq!(state.step, SetupStep::Connection);
        assert_eq!(state.selection.selected(), 2);
    }

    #[test]
    fn existing_profile_is_selected_without_mutating_a_candidate_profile() {
        let initial = ProviderProfile::default();
        let mut state = SetupState::new(FirstRunSetupRequest {
            initial_profile: initial.clone(),
            existing_profiles: vec![ExistingProfileChoice {
                name: "work".to_owned(),
                provider: "openai".to_owned(),
                model: "gpt-5".to_owned(),
            }],
        });
        state.selection.set_selected(2, state.choices().len());
        enter(&mut state);
        assert_eq!(state.step, SetupStep::ExistingProfile);
        enter(&mut state);
        assert_eq!(state.step, SetupStep::Review);
        let outcome = enter(&mut state).expect("outcome");
        assert_eq!(
            outcome,
            FirstRunSetupOutcome::UseExisting("work".to_owned())
        );
        assert_eq!(state.profile, initial);
    }

    #[test]
    fn escape_from_root_cancels_without_a_profile_result() {
        let mut state = SetupState::new(FirstRunSetupRequest {
            initial_profile: ProviderProfile::default(),
            existing_profiles: Vec::new(),
        });
        assert_eq!(state.escape(), Some(FirstRunSetupOutcome::Cancelled));
    }

    #[test]
    fn every_connection_preset_satisfies_profile_invariants() {
        for index in 0..6 {
            let mut state = SetupState::new(FirstRunSetupRequest {
                initial_profile: ProviderProfile::default(),
                existing_profiles: Vec::new(),
            });
            state.apply_connection(index);
            state.profile.configured = true;
            state.profile.validate().expect("preset must validate");
        }
    }

    #[test]
    fn clipping_is_resize_safe() {
        assert_eq!(clip_line("abcdef", 4), "abc…");
        assert_eq!(clip_line("abc", 4), "abc");
        assert_eq!(clip_line("abcdef", 1), "…");
    }
}
