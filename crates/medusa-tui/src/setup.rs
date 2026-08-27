use std::{
    collections::BTreeMap,
    io::{self, IsTerminal, Write},
    time::Duration,
};

use crossterm::{
    cursor::{Hide, MoveTo, Show},
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    execute, queue,
    style::{Attribute, Print, SetAttribute},
    terminal::{
        Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode,
        enable_raw_mode, size,
    },
};
use medusa_config::{
    ProviderCatalogEntry, ProviderProfile, apply_provider_defaults, provider_catalog,
    provider_catalog_entry, provider_catalog_entry_for_profile, provider_model_options,
};

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

/// A provider-owned browser sign-in attempt. Polling must be non-blocking.
pub trait BrowserOAuthSession {
    /// `None` means the attempt is still running. `Some(Ok(models))` means sign-in completed and
    /// may include provider-discovered model ids. `Some(Err(message))` is a bounded, redacted
    /// failure suitable for display in the setup UI.
    fn poll(&mut self) -> io::Result<Option<Result<Vec<String>, String>>>;

    /// Cancel helper/listener processes that are owned by this attempt.
    fn cancel(&mut self);
}

/// Host bridge for provider actions that must remain outside `medusa-tui`.
pub trait FirstRunSetupHost {
    fn start_browser_oauth(
        &mut self,
        provider_id: &str,
    ) -> Result<Box<dyn BrowserOAuthSession>, String>;
}

struct UnsupportedSetupHost;

impl FirstRunSetupHost for UnsupportedSetupHost {
    fn start_browser_oauth(
        &mut self,
        provider_id: &str,
    ) -> Result<Box<dyn BrowserOAuthSession>, String> {
        Err(format!(
            "browser sign-in is unavailable for provider `{provider_id}` in this host"
        ))
    }
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
    Provider,
    Authentication,
    Model,
    Speed,
    Reasoning,
    Review,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct SetupChoice {
    label: String,
    description: String,
}

#[derive(Debug, Eq, PartialEq)]
enum SetupTransition {
    None,
    StartBrowserOAuth(String),
    Finish(FirstRunSetupOutcome),
}

struct SetupState {
    profile: ProviderProfile,
    existing_profiles: Vec<ExistingProfileChoice>,
    step: SetupStep,
    history: Vec<SetupStep>,
    mode: Option<SetupMode>,
    selected_existing: Option<usize>,
    selection: SelectionState,
    discovered_models: BTreeMap<String, Vec<String>>,
    searching: bool,
    status: Option<String>,
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
            discovered_models: BTreeMap::new(),
            searching: false,
            status: None,
        }
    }

    fn provider_entry(&self) -> Option<&'static ProviderCatalogEntry> {
        provider_catalog_entry_for_profile(&self.profile).or_else(|| {
            provider_catalog()
                .iter()
                .find(|entry| entry.id == self.profile.provider)
        })
    }

    fn provider_index(&self) -> usize {
        let selected = self
            .provider_entry()
            .map(|entry| entry.id)
            .unwrap_or(self.profile.provider.as_str());
        provider_catalog()
            .iter()
            .position(|entry| entry.id == selected)
            .unwrap_or(0)
    }

    fn choices(&self) -> Vec<SetupChoice> {
        match self.step {
            SetupStep::Mode => {
                let mut choices = vec![
                    SetupChoice {
                        label: "Quick setup".to_owned(),
                        description: "Recommended route with the minimum required choices"
                            .to_owned(),
                    },
                    SetupChoice {
                        label: "Advanced setup".to_owned(),
                        description: "Choose provider, authentication, model, speed, and reasoning"
                            .to_owned(),
                    },
                ];
                if !self.existing_profiles.is_empty() {
                    choices.push(SetupChoice {
                        label: "Existing profile".to_owned(),
                        description: "Activate an already configured named profile".to_owned(),
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
            SetupStep::Provider => provider_catalog()
                .iter()
                .map(|entry| SetupChoice {
                    label: entry.display_name.to_owned(),
                    description: entry.disabled_reason.map_or_else(
                        || entry.description.to_owned(),
                        |reason| format!("Unavailable: {reason}"),
                    ),
                })
                .collect(),
            SetupStep::Authentication => self.authentication_choices(),
            SetupStep::Model => self
                .model_indices()
                .into_iter()
                .filter_map(|index| {
                    self.model_options().get(index).map(|model| SetupChoice {
                        label: model.clone(),
                        description: self.provider_entry().map_or_else(
                            || "Current custom model".to_owned(),
                            |entry| {
                                if entry.discover_models
                                    && self
                                        .discovered_models
                                        .get(entry.id)
                                        .is_some_and(|models| models.contains(model))
                                {
                                    "Discovered from the authenticated provider".to_owned()
                                } else if model == entry.default_model {
                                    "Recommended model".to_owned()
                                } else {
                                    "Available catalog/current model".to_owned()
                                }
                            },
                        ),
                    })
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
                description: format!("Speed policy: {value}"),
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
                description: format!("Reasoning level: {value}"),
            })
            .collect(),
            SetupStep::Review => vec![
                SetupChoice {
                    label: if self.selected_existing.is_some() {
                        "Use profile and continue".to_owned()
                    } else {
                        "Save and continue".to_owned()
                    },
                    description: "Validate through the existing ProviderProfileCatalog authority"
                        .to_owned(),
                },
                SetupChoice {
                    label: "Back".to_owned(),
                    description: "Return to the previous setup screen".to_owned(),
                },
            ],
        }
    }

    fn authentication_choices(&self) -> Vec<SetupChoice> {
        let Some(entry) = self.provider_entry() else {
            return vec![SetupChoice {
                label: "Existing credentials".to_owned(),
                description: "Keep the current custom provider credential mode".to_owned(),
            }];
        };
        if entry.browser_oauth {
            return vec![SetupChoice {
                label: "Sign in with browser".to_owned(),
                description:
                    "Open ChatGPT sign-in and return here after Codex confirms the account"
                        .to_owned(),
            }];
        }
        entry
            .auth_methods
            .iter()
            .map(|method| SetupChoice {
                label: auth_label(method).to_owned(),
                description: auth_description(method).to_owned(),
            })
            .collect()
    }

    fn model_options(&self) -> Vec<String> {
        let provider = self
            .provider_entry()
            .map(|entry| entry.id)
            .unwrap_or(self.profile.provider.as_str());
        let discovered = self
            .discovered_models
            .get(provider)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        provider_model_options(provider, &self.profile.model, discovered)
    }

    fn model_indices(&self) -> Vec<usize> {
        let models = self.model_options();
        self.selection.filtered_indices(&models)
    }

    fn title(&self) -> &'static str {
        match self.step {
            SetupStep::Mode => "Welcome to Medusa",
            SetupStep::ExistingProfile => "Choose an existing profile",
            SetupStep::Provider => "Choose a provider",
            SetupStep::Authentication => "Authentication",
            SetupStep::Model => "Choose a model",
            SetupStep::Speed => "Choose speed",
            SetupStep::Reasoning => "Choose reasoning",
            SetupStep::Review => "Review first-run setup",
        }
    }

    fn subtitle(&self) -> String {
        if let Some(status) = &self.status {
            return status.clone();
        }
        match self.step {
            SetupStep::Mode => {
                "First-run setup uses the same typed provider catalog as in-session model selection."
                    .to_owned()
            }
            SetupStep::ExistingProfile => {
                "The selected profile becomes active only after validation succeeds.".to_owned()
            }
            SetupStep::Provider => {
                "Provider metadata, defaults, authentication, and models come from medusa-config."
                    .to_owned()
            }
            SetupStep::Authentication => {
                "Credentials stay with the provider, environment, or Codex app-server; Medusa does not display them."
                    .to_owned()
            }
            SetupStep::Model if self.searching => {
                format!("Search: {}", self.selection.search())
            }
            SetupStep::Model => "Press / to filter long model lists.".to_owned(),
            SetupStep::Speed => "This updates the existing ProviderProfile speed setting.".to_owned(),
            SetupStep::Reasoning => {
                "This updates the existing ProviderProfile reasoning setting.".to_owned()
            }
            SetupStep::Review => {
                "The candidate is still staged; cancellation leaves the active profile unchanged."
                    .to_owned()
            }
        }
    }

    fn move_selection(&mut self, delta: isize) {
        if self.step == SetupStep::Model {
            let indices = self.model_indices();
            self.selection.move_in_with(&indices, delta, |_| true);
        } else {
            self.selection.move_by(self.choices().len(), delta);
        }
    }

    fn enter(&mut self) -> SetupTransition {
        self.status = None;
        let selected = self.selection.selected();
        match self.step {
            SetupStep::Mode => {
                match selected {
                    0 => {
                        self.mode = Some(SetupMode::Quick);
                        self.selected_existing = None;
                        self.go_to(SetupStep::Provider);
                        let recommended = provider_catalog()
                            .iter()
                            .position(|entry| entry.id == "omniroute")
                            .unwrap_or(0);
                        self.selection = SelectionState::new(recommended);
                    }
                    1 => {
                        self.mode = Some(SetupMode::Advanced);
                        self.selected_existing = None;
                        self.go_to(SetupStep::Provider);
                    }
                    2 if !self.existing_profiles.is_empty() => {
                        self.mode = None;
                        self.go_to(SetupStep::ExistingProfile);
                    }
                    _ => {}
                }
                SetupTransition::None
            }
            SetupStep::ExistingProfile => {
                if selected < self.existing_profiles.len() {
                    self.selected_existing = Some(selected);
                    self.go_to(SetupStep::Review);
                }
                SetupTransition::None
            }
            SetupStep::Provider => {
                let Some(entry) = provider_catalog().get(selected) else {
                    return SetupTransition::None;
                };
                if let Some(reason) = entry.disabled_reason {
                    self.status = Some(format!("{} is unavailable: {reason}", entry.display_name));
                    return SetupTransition::None;
                }
                apply_provider_defaults(entry, &mut self.profile);
                if entry.browser_oauth || entry.auth_methods.len() > 1 {
                    self.go_to(SetupStep::Authentication);
                } else {
                    self.go_to(SetupStep::Model);
                }
                SetupTransition::None
            }
            SetupStep::Authentication => {
                let Some(entry) = self.provider_entry() else {
                    self.go_to(SetupStep::Model);
                    return SetupTransition::None;
                };
                if entry.browser_oauth {
                    self.status =
                        Some("Waiting for browser sign-in… Press Esc to cancel.".to_owned());
                    return SetupTransition::StartBrowserOAuth(entry.id.to_owned());
                }
                if let Some(method) = entry.auth_methods.get(selected) {
                    self.profile.auth = (*method).to_owned();
                    self.go_to(SetupStep::Model);
                }
                SetupTransition::None
            }
            SetupStep::Model => {
                let models = self.model_options();
                if let Some(model) = models.get(selected) {
                    self.profile.model.clone_from(model);
                }
                self.searching = false;
                self.selection.clear_search();
                if self.mode == Some(SetupMode::Advanced) {
                    self.go_to(SetupStep::Speed);
                } else {
                    self.go_to(SetupStep::Review);
                }
                SetupTransition::None
            }
            SetupStep::Speed => {
                if let Some(value) = ["fast", "balanced", "quality", "custom"].get(selected) {
                    self.profile.speed = (*value).to_owned();
                }
                self.go_to(SetupStep::Reasoning);
                SetupTransition::None
            }
            SetupStep::Reasoning => {
                if let Some(value) = ["low", "medium", "high", "maximum"].get(selected) {
                    self.profile.reasoning = (*value).to_owned();
                }
                self.go_to(SetupStep::Review);
                SetupTransition::None
            }
            SetupStep::Review => {
                if selected == 1 {
                    self.back();
                    return SetupTransition::None;
                }
                if let Some(index) = self.selected_existing {
                    return self
                        .existing_profiles
                        .get(index)
                        .map(|profile| {
                            SetupTransition::Finish(FirstRunSetupOutcome::UseExisting(
                                profile.name.clone(),
                            ))
                        })
                        .unwrap_or(SetupTransition::None);
                }
                let mut profile = self.profile.clone();
                profile.configured = true;
                SetupTransition::Finish(FirstRunSetupOutcome::Configure(profile))
            }
        }
    }

    fn oauth_succeeded(&mut self, provider_id: &str, models: Vec<String>) {
        if !models.is_empty() && !models.iter().any(|model| model == &self.profile.model) {
            self.profile.model.clone_from(&models[0]);
        }
        self.discovered_models
            .insert(provider_id.to_owned(), models);
        self.status = Some("Browser sign-in succeeded; provider models were refreshed.".to_owned());
        self.go_to(SetupStep::Model);
    }

    fn oauth_failed(&mut self, message: String) {
        self.status = Some(format!("Browser sign-in failed: {message}"));
    }

    fn escape(&mut self) -> SetupTransition {
        self.status = None;
        if self.searching {
            self.searching = false;
            self.selection.clear_search();
            self.normalize_model_selection();
            return SetupTransition::None;
        }
        if self.step == SetupStep::Mode {
            return SetupTransition::Finish(FirstRunSetupOutcome::Cancelled);
        }
        self.back();
        SetupTransition::None
    }

    fn start_search(&mut self) {
        if self.step == SetupStep::Model {
            self.searching = true;
            self.selection.clear_search();
        }
    }

    fn push_search(&mut self, character: char) {
        if self.step == SetupStep::Model && self.searching {
            self.selection.push_search(character);
            self.normalize_model_selection();
        }
    }

    fn pop_search(&mut self) {
        if self.step == SetupStep::Model && self.searching {
            self.selection.pop_search();
            self.normalize_model_selection();
        }
    }

    fn normalize_model_selection(&mut self) {
        let indices = self.model_indices();
        if !indices.contains(&self.selection.selected())
            && let Some(first) = indices.first().copied()
        {
            self.selection
                .set_selected(first, self.model_options().len());
        }
    }

    fn go_to(&mut self, next: SetupStep) {
        self.history.push(self.step);
        self.step = next;
        self.searching = false;
        self.selection.clear_search();
        self.reset_selection_for_step();
    }

    fn back(&mut self) {
        if let Some(previous) = self.history.pop() {
            self.step = previous;
            self.searching = false;
            self.selection.clear_search();
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
            SetupStep::Provider => self.provider_index(),
            SetupStep::Authentication => self
                .provider_entry()
                .and_then(|entry| {
                    entry
                        .auth_methods
                        .iter()
                        .position(|method| *method == self.profile.auth)
                })
                .unwrap_or(0),
            SetupStep::Model => self
                .model_options()
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
            SetupStep::Review => 0,
        };
        self.selection = SelectionState::new(selected);
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
                "The existing named profile remains secret-free and revision checked.".to_owned(),
            ];
        }
        let display_name = self
            .provider_entry()
            .map(|entry| entry.display_name)
            .unwrap_or(self.profile.provider.as_str());
        let mut lines = vec![
            format!("Provider:   {display_name}"),
            format!("Route:      {}", self.profile.connection),
            format!("Model:      {}", self.profile.model),
            format!("Speed:      {}", self.profile.speed),
            format!("Reasoning:  {}", self.profile.reasoning),
            format!("Auth:       {}", self.profile.auth),
        ];
        if let Some(base_url) = self.profile.base_url.as_deref() {
            lines.push(format!("Base URL:   {base_url}"));
        }
        lines.push(String::new());
        lines.push("No credential value is written to provider.toml.".to_owned());
        lines
    }
}

pub fn run_first_run_setup(request: FirstRunSetupRequest) -> io::Result<FirstRunSetupOutcome> {
    let mut host = UnsupportedSetupHost;
    run_first_run_setup_with_host(request, &mut host)
}

pub fn run_first_run_setup_with_host(
    request: FirstRunSetupRequest,
    host: &mut dyn FirstRunSetupHost,
) -> io::Result<FirstRunSetupOutcome> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "first-run setup requires an interactive terminal",
        ));
    }

    let mut terminal = SetupTerminal::enter()?;
    let mut state = SetupState::new(request);
    let mut oauth_session: Option<Box<dyn BrowserOAuthSession>> = None;
    loop {
        terminal.render(&state)?;

        if let Some(session) = oauth_session.as_mut() {
            if let Some(result) = session.poll()? {
                oauth_session = None;
                match result {
                    Ok(models) => {
                        let provider = state
                            .provider_entry()
                            .map(|entry| entry.id)
                            .unwrap_or("openai-oauth")
                            .to_owned();
                        state.oauth_succeeded(&provider, models);
                    }
                    Err(message) => state.oauth_failed(message),
                }
                continue;
            }
            if !event::poll(Duration::from_millis(100))? {
                continue;
            }
            let Event::Key(key) = event::read()? else {
                continue;
            };
            if key.kind == KeyEventKind::Release {
                continue;
            }
            if key.code == KeyCode::Esc
                || (key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL))
            {
                session.cancel();
                oauth_session = None;
                state.status =
                    Some("Browser sign-in cancelled; configuration unchanged.".to_owned());
            }
            continue;
        }

        let Event::Key(key) = event::read()? else {
            continue;
        };
        if key.kind == KeyEventKind::Release {
            continue;
        }

        if state.searching {
            match key.code {
                KeyCode::Esc => {
                    state.escape();
                }
                KeyCode::Backspace => state.pop_search(),
                KeyCode::Up => state.move_selection(-1),
                KeyCode::Down => state.move_selection(1),
                KeyCode::Enter => {
                    if let SetupTransition::Finish(outcome) = state.enter() {
                        terminal.restore();
                        return Ok(outcome);
                    }
                }
                KeyCode::Char(character)
                    if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT =>
                {
                    state.push_search(character);
                }
                _ => {}
            }
            continue;
        }

        let transition = match key.code {
            KeyCode::Up => {
                state.move_selection(-1);
                SetupTransition::None
            }
            KeyCode::Down => {
                state.move_selection(1);
                SetupTransition::None
            }
            KeyCode::Home => {
                state.selection.set_selected(0, state.choices().len());
                SetupTransition::None
            }
            KeyCode::End => {
                let count = if state.step == SetupStep::Model {
                    state.model_options().len()
                } else {
                    state.choices().len()
                };
                state.selection.set_selected(count.saturating_sub(1), count);
                SetupTransition::None
            }
            KeyCode::Enter => state.enter(),
            KeyCode::Esc => state.escape(),
            KeyCode::Char('/') if key.modifiers.is_empty() => {
                state.start_search();
                SetupTransition::None
            }
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                SetupTransition::Finish(FirstRunSetupOutcome::Cancelled)
            }
            _ => SetupTransition::None,
        };

        match transition {
            SetupTransition::None => {}
            SetupTransition::Finish(outcome) => {
                terminal.restore();
                return Ok(outcome);
            }
            SetupTransition::StartBrowserOAuth(provider_id) => {
                match host.start_browser_oauth(&provider_id) {
                    Ok(session) => oauth_session = Some(session),
                    Err(message) => state.oauth_failed(message),
                }
            }
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
            Print(clip_line(&state.subtitle(), width)),
            Print("\r\n\r\n")
        )?;

        if state.step == SetupStep::Review {
            for line in state.review_lines() {
                queue!(self.stdout, Print(clip_line(&line, width)), Print("\r\n"))?;
            }
            queue!(self.stdout, Print("\r\n"))?;
        }

        let choices = state.choices();
        let selected_raw = state.selection.selected();
        let visible_model_indices = (state.step == SetupStep::Model).then(|| state.model_indices());
        for (visible_index, choice) in choices.iter().enumerate() {
            let selected = if let Some(indices) = &visible_model_indices {
                indices.get(visible_index).copied() == Some(selected_raw)
            } else {
                visible_index == selected_raw
            };
            let marker = if selected { "›" } else { " " };
            if selected {
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
                "↑/↓ move · Enter select · / search models · Esc back/cancel · Ctrl+C cancel",
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

fn auth_label(method: &str) -> &'static str {
    match method {
        "oauth" => "Provider OAuth",
        "api-key" => "API key from environment",
        "existing" => "Existing provider credentials",
        "none" => "No authentication",
        _ => "Authentication",
    }
}

fn auth_description(method: &str) -> &'static str {
    match method {
        "oauth" => "Use the provider's OAuth authority",
        "api-key" => "Read the provider's registered environment variable; never store the value",
        "existing" => "Use credentials already owned by the selected provider",
        "none" => "The route does not require a Medusa-managed credential",
        _ => "Keep the route's typed authentication mode",
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

    struct ImmediateOAuth {
        result: Option<Result<Vec<String>, String>>,
        cancelled: bool,
    }

    impl BrowserOAuthSession for ImmediateOAuth {
        fn poll(&mut self) -> io::Result<Option<Result<Vec<String>, String>>> {
            Ok(self.result.take())
        }

        fn cancel(&mut self) {
            self.cancelled = true;
        }
    }

    fn enter(state: &mut SetupState) -> SetupTransition {
        state.enter()
    }

    #[test]
    fn quick_setup_uses_catalog_recommended_route() {
        let mut state = SetupState::new(FirstRunSetupRequest {
            initial_profile: ProviderProfile::default(),
            existing_profiles: Vec::new(),
        });
        assert_eq!(enter(&mut state), SetupTransition::None);
        assert_eq!(state.step, SetupStep::Provider);
        let selected = provider_catalog()[state.selection.selected()].id;
        assert_eq!(selected, "omniroute");
        enter(&mut state);
        assert_eq!(state.profile.connection, "omniroute");
        assert_eq!(state.profile.provider, "auto/coding");
    }

    #[test]
    fn oauth_route_requests_browser_sign_in_before_model_selection() {
        let mut state = SetupState::new(FirstRunSetupRequest {
            initial_profile: ProviderProfile::default(),
            existing_profiles: Vec::new(),
        });
        state.mode = Some(SetupMode::Advanced);
        state.step = SetupStep::Provider;
        let oauth = provider_catalog()
            .iter()
            .position(|entry| entry.id == "openai-oauth")
            .expect("oauth entry");
        state
            .selection
            .set_selected(oauth, provider_catalog().len());
        enter(&mut state);
        assert_eq!(state.step, SetupStep::Authentication);
        assert_eq!(
            enter(&mut state),
            SetupTransition::StartBrowserOAuth("openai-oauth".to_owned())
        );
    }

    #[test]
    fn discovered_oauth_models_merge_with_current_value() {
        let mut state = SetupState::new(FirstRunSetupRequest {
            initial_profile: ProviderProfile::default(),
            existing_profiles: Vec::new(),
        });
        let entry = provider_catalog_entry("openai-oauth").expect("oauth");
        apply_provider_defaults(entry, &mut state.profile);
        state.oauth_succeeded(
            "openai-oauth",
            vec!["gpt-live".to_owned(), "gpt-5.6-luna".to_owned()],
        );
        let models = state.model_options();
        assert!(models.contains(&"gpt-live".to_owned()));
        assert!(models.contains(&"gpt-5.6-luna".to_owned()));
        assert_eq!(state.profile.model, "gpt-5.6-luna");
    }

    #[test]
    fn discovered_oauth_models_replace_unavailable_fallback() {
        let mut state = SetupState::new(FirstRunSetupRequest {
            initial_profile: ProviderProfile::default(),
            existing_profiles: Vec::new(),
        });
        let entry = provider_catalog_entry("openai-oauth").expect("oauth");
        apply_provider_defaults(entry, &mut state.profile);
        state.oauth_succeeded(
            "openai-oauth",
            vec!["gpt-account-a".to_owned(), "gpt-account-b".to_owned()],
        );
        assert_eq!(state.profile.model, "gpt-account-a");
        assert_eq!(state.selection.selected(), 0);
    }

    #[test]
    fn model_search_filters_without_losing_underlying_selection() {
        let mut state = SetupState::new(FirstRunSetupRequest {
            initial_profile: ProviderProfile::default(),
            existing_profiles: Vec::new(),
        });
        state.step = SetupStep::Model;
        state.start_search();
        for character in "M2.7-high".chars() {
            state.push_search(character);
        }
        let choices = state.choices();
        assert_eq!(choices.len(), 1);
        assert!(choices[0].label.contains("M2.7-highspeed"));
    }

    #[test]
    fn existing_profile_is_selected_without_candidate_mutation() {
        let initial = ProviderProfile::default();
        let mut state = SetupState::new(FirstRunSetupRequest {
            initial_profile: initial.clone(),
            existing_profiles: vec![ExistingProfileChoice {
                name: "work".to_owned(),
                provider: "openai".to_owned(),
                model: "gpt-5.1-codex".to_owned(),
            }],
        });
        state.selection.set_selected(2, state.choices().len());
        enter(&mut state);
        enter(&mut state);
        assert_eq!(state.step, SetupStep::Review);
        assert_eq!(
            enter(&mut state),
            SetupTransition::Finish(FirstRunSetupOutcome::UseExisting("work".to_owned()))
        );
        assert_eq!(state.profile, initial);
    }

    #[test]
    fn escape_from_root_cancels_without_profile_result() {
        let mut state = SetupState::new(FirstRunSetupRequest {
            initial_profile: ProviderProfile::default(),
            existing_profiles: Vec::new(),
        });
        assert_eq!(
            state.escape(),
            SetupTransition::Finish(FirstRunSetupOutcome::Cancelled)
        );
    }

    #[test]
    fn every_catalog_route_produces_valid_profile_defaults() {
        for entry in provider_catalog() {
            let mut profile = ProviderProfile::default();
            apply_provider_defaults(entry, &mut profile);
            profile.configured = true;
            profile.validate().expect(entry.id);
        }
    }

    #[test]
    fn clipping_is_resize_safe() {
        assert_eq!(clip_line("abcdef", 4), "abc…");
        assert_eq!(clip_line("abc", 4), "abc");
        assert_eq!(clip_line("abcdef", 1), "…");
    }
}
