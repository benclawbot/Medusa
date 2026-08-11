use std::{collections::BTreeMap, path::Path};

use medusa_config::{
    Config, ConfigDoctorCheck, ConfigDoctorReport, ConfigurationApplyTiming,
    ConfigurationChangeOrigin, ProviderProfile, ProviderProfileCatalog, StagedProviderProfile,
    apply_provider_defaults, diagnose_config_catalog, provider_catalog_entry,
    provider_ids_with_current, provider_model_options, repair_config_check,
};

use crate::{
    clipboard::PromptDraft,
    commands::{ConfigCommand, Effort, ModelConfiguration, SlashCommand},
    input::SelectionState,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TranscriptEntry {
    User(PromptDraft),
    Assistant(String),
    Activity(TranscriptActivity),
    System(String),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TranscriptActivityKind {
    Assistant,
    Done,
    Error,
    Progress,
    Tool,
    Verification,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TranscriptActivity {
    pub id: Option<String>,
    pub kind: TranscriptActivityKind,
    pub title: String,
    pub details: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TranscriptPlan {
    pub steps: Vec<TranscriptPlanStep>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TranscriptPlanStep {
    pub title: String,
    pub state: TranscriptPlanStepState,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TranscriptPlanStepState {
    Pending,
    Active,
    Completed,
    Failed,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AppAction {
    None,
    Redraw,
    Submit(PromptDraft),
    AnswerQuestion(String),
    Command(SlashCommand),
    ConfigureModel(ModelConfiguration),
    Interrupt,
    Quit,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TerminalPosition {
    pub row: u16,
    pub column: u16,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TextSelection {
    pub anchor: TerminalPosition,
    pub active: TerminalPosition,
}

impl TextSelection {
    #[must_use]
    pub fn ordered(self) -> (TerminalPosition, TerminalPosition) {
        if (self.anchor.row, self.anchor.column) <= (self.active.row, self.active.column) {
            (self.anchor, self.active)
        } else {
            (self.active, self.anchor)
        }
    }

    #[must_use]
    pub fn is_empty(self) -> bool {
        self.anchor == self.active
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QuestionOption {
    pub label: String,
    pub description: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QuestionPrompt {
    pub header: String,
    pub question: String,
    pub options: Vec<QuestionOption>,
    pub multi_select: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Default)]
struct QuestionAnswer {
    selected_options: Vec<usize>,
    custom_answer: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QuestionModal {
    questions: Vec<QuestionPrompt>,
    answers: Vec<QuestionAnswer>,
    option_selections: Vec<SelectionState>,
    active_question: usize,
    reviewing: bool,
}

impl QuestionModal {
    pub fn new(questions: Vec<QuestionPrompt>) -> Self {
        let count = questions.len();
        Self {
            questions,
            answers: vec![QuestionAnswer::default(); count],
            option_selections: vec![SelectionState::default(); count],
            active_question: 0,
            reviewing: false,
        }
    }

    #[must_use]
    pub fn questions(&self) -> &[QuestionPrompt] {
        &self.questions
    }

    #[must_use]
    pub fn active_question(&self) -> usize {
        self.active_question
    }

    #[must_use]
    pub fn is_reviewing(&self) -> bool {
        self.reviewing
    }

    #[must_use]
    pub fn active_prompt(&self) -> Option<&QuestionPrompt> {
        self.questions.get(self.active_question)
    }

    #[must_use]
    pub fn active_selected_option(&self) -> usize {
        self.option_selections
            .get(self.active_question)
            .map(SelectionState::selected)
            .unwrap_or_default()
    }

    #[must_use]
    pub fn active_custom_answer(&self) -> &str {
        self.answers
            .get(self.active_question)
            .map(|answer| answer.custom_answer.as_str())
            .unwrap_or_default()
    }

    pub(super) fn move_selection(&mut self, delta: isize) {
        let active_question = self.active_question;
        let option_count = self
            .questions
            .get(active_question)
            .map_or(0, |prompt| prompt.options.len());
        if let Some(selection) = self.option_selections.get_mut(active_question) {
            selection.move_by(option_count, delta);
        }
    }

    pub(super) fn move_question(&mut self, delta: isize) {
        if self.questions.is_empty() {
            return;
        }
        if self.reviewing {
            self.reviewing = false;
            self.active_question = self.questions.len().saturating_sub(1);
            return;
        }
        self.active_question = cycle_index(self.active_question, self.questions.len(), delta);
    }

    pub(super) fn back_one_question(&mut self) -> bool {
        if self.reviewing {
            self.reviewing = false;
            self.active_question = self.questions.len().saturating_sub(1);
            return true;
        }
        if self.active_question > 0 {
            self.active_question -= 1;
            return true;
        }
        false
    }

    pub(super) fn advance_or_review(&mut self) {
        if self.active_question.saturating_add(1) < self.questions.len() {
            self.active_question = self.active_question.saturating_add(1);
        } else {
            self.reviewing = true;
        }
    }

    pub(super) fn toggle_current_option(&mut self) {
        let active_question = self.active_question;
        let Some((option_count, multi_select)) = self
            .questions
            .get(active_question)
            .map(|prompt| (prompt.options.len(), prompt.multi_select))
        else {
            return;
        };
        if option_count == 0 || !multi_select {
            return;
        }
        let selected = self.active_selected_option();
        let answer = &mut self.answers[active_question];
        if let Some(position) = answer
            .selected_options
            .iter()
            .position(|option| *option == selected)
        {
            answer.selected_options.remove(position);
        } else {
            answer.selected_options.push(selected);
            answer.selected_options.sort_unstable();
        }
    }

    pub(super) fn select_current_answer(&mut self) -> bool {
        let active_question = self.active_question;
        let Some((option_count, multi_select)) = self
            .questions
            .get(active_question)
            .map(|prompt| (prompt.options.len(), prompt.multi_select))
        else {
            return false;
        };
        if option_count > 0 && !multi_select {
            let selected = self.active_selected_option();
            self.answers[active_question].selected_options = vec![selected];
        } else if option_count > 0 && self.answer_for(active_question).is_none() {
            self.toggle_current_option();
        }
        self.answer_for(active_question).is_some()
    }

    pub(super) fn insert_answer(&mut self, text: &str) {
        if let Some(answer) = self.answers.get_mut(self.active_question) {
            answer.custom_answer.push_str(text);
        }
    }

    pub(super) fn delete_answer_character(&mut self) {
        if let Some(answer) = self.answers.get_mut(self.active_question) {
            answer.custom_answer.pop();
        }
    }

    #[must_use]
    pub fn answer_for(&self, index: usize) -> Option<String> {
        let prompt = self.questions.get(index)?;
        let answer = self.answers.get(index)?;
        (!answer.custom_answer.trim().is_empty())
            .then(|| answer.custom_answer.trim().to_owned())
            .or_else(|| {
                let labels = answer
                    .selected_options
                    .iter()
                    .filter_map(|option| prompt.options.get(*option))
                    .map(|option| option.label.as_str())
                    .collect::<Vec<_>>();
                (!labels.is_empty()).then(|| labels.join(", "))
            })
    }

    pub(super) fn answer_bundle(&self) -> Option<String> {
        let answers = self
            .questions
            .iter()
            .enumerate()
            .map(|(index, prompt)| {
                self.answer_for(index)
                    .map(|answer| format!("{}: {answer}", prompt.header))
            })
            .collect::<Option<Vec<_>>>()?;
        Some(answers.join("\n"))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ModelModalFocus {
    Provider,
    Model,
    Effort,
    ApiKey,
    Apply,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SettingsPage {
    Root,
    Profile,
    Provider,
    Model,
    Speed,
    Reasoning,
    Authentication,
    BaseUrl,
    Status,
    Review,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SettingsChoice {
    pub label: String,
    pub description: String,
    pub enabled: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct SettingsState {
    catalog: ProviderProfileCatalog,
    revision: u64,
    active_profile: String,
    profile: ProviderProfile,
    staged: StagedProviderProfile,
    profiles: Vec<String>,
    last_apply_timing: Option<ConfigurationApplyTiming>,
    page: SettingsPage,
    root_selection: SelectionState,
    choice_selection: SelectionState,
    searching: bool,
    base_url_edit: String,
    doctor: ConfigDoctorReport,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelModal {
    provider_options: Vec<String>,
    provider_selection: SelectionState,
    model_selection: SelectionState,
    model_options: Vec<String>,
    effort: Effort,
    focus: ModelModalFocus,
    api_key: String,
    has_existing_key: bool,
    settings: Option<SettingsState>,
}

impl ModelModal {
    pub(super) fn new(
        model_label: Option<&str>,
        effort_label: Option<&str>,
        has_existing_key: bool,
    ) -> Self {
        let (provider, current_model) = model_label
            .and_then(|label| label.split_once(" / "))
            .unwrap_or(("minimax", "MiniMax-M3"));
        let provider_options = provider_ids_with_current(provider);
        let provider_index = provider_options
            .iter()
            .position(|candidate| candidate == provider)
            .unwrap_or(0);
        let selected_provider = provider_options
            .get(provider_index)
            .map(String::as_str)
            .unwrap_or("minimax");
        let models = provider_model_options(selected_provider, current_model, &[]);
        let model_index = models
            .iter()
            .position(|candidate| candidate == current_model)
            .unwrap_or(0);
        Self {
            provider_options,
            provider_selection: SelectionState::new(provider_index),
            model_selection: SelectionState::new(model_index),
            model_options: models,
            effort: effort_from_label(effort_label),
            focus: ModelModalFocus::Model,
            api_key: String::new(),
            has_existing_key,
            settings: None,
        }
    }

    pub(super) fn new_settings(
        model_label: Option<&str>,
        effort_label: Option<&str>,
        has_existing_key: bool,
    ) -> Result<Self, String> {
        let catalog = ProviderProfileCatalog::user().map_err(|error| error.to_string())?;
        Self::new_settings_with_catalog(
            model_label,
            effort_label,
            has_existing_key,
            catalog,
        )
    }

    pub(super) fn new_settings_with_catalog(
        model_label: Option<&str>,
        effort_label: Option<&str>,
        has_existing_key: bool,
        catalog: ProviderProfileCatalog,
    ) -> Result<Self, String> {
        let snapshot = catalog.snapshot().map_err(|error| error.to_string())?;
        let staged = StagedProviderProfile::from_snapshot(snapshot.clone());
        let profiles = catalog
            .list()
            .map_err(|error| error.to_string())?
            .into_iter()
            .map(|summary| summary.name)
            .collect::<Vec<_>>();
        let doctor = diagnose_config_catalog(&catalog).map_err(|error| error.to_string())?;
        let last_apply_timing = catalog
            .last_change()
            .map_err(|error| error.to_string())?
            .map(|change| change.apply_timing);
        let mut modal = Self::new(model_label, effort_label, has_existing_key);
        let active_profile = snapshot.active_profile;
        let profile = snapshot.profile;
        let profile_index = profiles
            .iter()
            .position(|name| name == &active_profile)
            .unwrap_or(0);
        modal.settings = Some(SettingsState {
            catalog,
            revision: snapshot.revision,
            active_profile,
            base_url_edit: profile.base_url.clone().unwrap_or_default(),
            profile,
            staged,
            profiles,
            last_apply_timing,
            page: SettingsPage::Root,
            root_selection: SelectionState::new(0),
            choice_selection: SelectionState::new(profile_index),
            searching: false,
            doctor,
        });
        Ok(modal)
    }

    #[must_use]
    pub fn is_settings(&self) -> bool {
        self.settings.is_some()
    }

    #[must_use]
    pub fn provider(&self) -> &str {
        self.provider_options
            .get(self.provider_selection.selected())
            .map(String::as_str)
            .unwrap_or("minimax")
    }

    #[must_use]
    pub fn model_options(&self) -> Vec<String> {
        self.model_options.clone()
    }

    #[must_use]
    pub fn selected_model(&self) -> String {
        self.model_options
            .get(self.model_selection.selected())
            .cloned()
            .unwrap_or_else(|| "MiniMax-M3".to_owned())
    }

    #[must_use]
    pub const fn selected_model_index(&self) -> usize {
        self.model_selection.selected()
    }

    #[must_use]
    pub const fn effort(&self) -> Effort {
        self.effort
    }

    #[must_use]
    pub const fn focus(&self) -> ModelModalFocus {
        self.focus
    }

    #[must_use]
    pub fn api_key_mask(&self) -> String {
        if self.api_key.is_empty() {
            if self.has_existing_key {
                "configured".to_owned()
            } else {
                "not configured".to_owned()
            }
        } else {
            "*".repeat(self.api_key.chars().count())
        }
    }

    #[must_use]
    pub fn configuration(&self) -> ModelConfiguration {
        ModelConfiguration {
            provider: self.provider().to_owned(),
            model: self.selected_model(),
            effort: self.effort,
            api_key: (!self.api_key.is_empty()).then(|| self.api_key.clone()),
        }
    }

    pub(super) fn cycle_focus(&mut self) {
        self.focus = match self.focus {
            ModelModalFocus::Provider => ModelModalFocus::Model,
            ModelModalFocus::Model => ModelModalFocus::Effort,
            ModelModalFocus::Effort => ModelModalFocus::ApiKey,
            ModelModalFocus::ApiKey => ModelModalFocus::Apply,
            ModelModalFocus::Apply => ModelModalFocus::Provider,
        };
    }

    pub(super) fn cycle_focus_back(&mut self) {
        self.focus = match self.focus {
            ModelModalFocus::Provider => ModelModalFocus::Provider,
            ModelModalFocus::Model => ModelModalFocus::Provider,
            ModelModalFocus::Effort => ModelModalFocus::Model,
            ModelModalFocus::ApiKey => ModelModalFocus::Effort,
            ModelModalFocus::Apply => ModelModalFocus::ApiKey,
        };
    }

    pub(super) fn focus_api_key(&mut self) {
        self.focus = ModelModalFocus::ApiKey;
    }

    pub(super) fn move_selection(&mut self, delta: isize) {
        match self.focus {
            ModelModalFocus::Provider => {
                self.provider_selection
                    .move_by(self.provider_options.len(), delta);
                let provider = self.provider().to_owned();
                self.model_options = provider_model_options(&provider, "", &[]);
                self.model_selection
                    .set_selected(0, self.model_options.len());
            }
            ModelModalFocus::Model => {
                self.model_selection
                    .move_by(self.model_options.len(), delta);
            }
            ModelModalFocus::Effort => {
                const EFFORTS: [Effort; 4] =
                    [Effort::Low, Effort::Medium, Effort::High, Effort::Auto];
                let index = EFFORTS
                    .iter()
                    .position(|candidate| *candidate == self.effort)
                    .unwrap_or(2);
                self.effort = EFFORTS[cycle_index(index, EFFORTS.len(), delta)];
            }
            ModelModalFocus::ApiKey | ModelModalFocus::Apply => {}
        }
    }

    pub(super) fn insert_key_text(&mut self, text: &str) {
        self.api_key
            .extend(text.chars().filter(|character| !character.is_whitespace()));
    }

    pub(super) fn delete_key_character(&mut self) {
        self.api_key.pop();
    }

    #[must_use]
    pub fn settings_page(&self) -> Option<SettingsPage> {
        self.settings.as_ref().map(|settings| settings.page)
    }

    #[must_use]
    pub fn settings_revision(&self) -> Option<u64> {
        self.settings.as_ref().map(|settings| settings.revision)
    }

    #[must_use]
    pub fn settings_active_profile(&self) -> Option<&str> {
        self.settings
            .as_ref()
            .map(|settings| settings.active_profile.as_str())
    }

    #[must_use]
    pub fn settings_last_apply_timing(&self) -> Option<ConfigurationApplyTiming> {
        self.settings
            .as_ref()
            .and_then(|settings| settings.last_apply_timing)
    }

    #[must_use]
    pub fn settings_root_selected(&self) -> usize {
        self.settings
            .as_ref()
            .map(|settings| settings.root_selection.selected())
            .unwrap_or_default()
    }

    #[must_use]
    pub fn settings_root_rows(&self) -> Vec<(String, String)> {
        let Some(settings) = self.settings.as_ref() else {
            return Vec::new();
        };
        vec![
            ("Profile".to_owned(), settings.active_profile.clone()),
            ("Provider".to_owned(), settings.profile.provider.clone()),
            ("Model".to_owned(), settings.profile.model.clone()),
            ("Speed".to_owned(), settings.profile.speed.clone()),
            ("Reasoning".to_owned(), settings.profile.reasoning.clone()),
            ("Authentication".to_owned(), settings.profile.auth.clone()),
            (
                "Base URL".to_owned(),
                settings
                    .profile
                    .base_url
                    .clone()
                    .unwrap_or_else(|| "provider default".to_owned()),
            ),
            (
                "Status".to_owned(),
                settings.doctor.summary_label().to_owned(),
            ),
            (
                "Review changes".to_owned(),
                if settings.staged.is_dirty() {
                    format!("{} pending", settings.staged.diff().len())
                } else {
                    "no changes".to_owned()
                },
            ),
        ]
    }

    pub(super) fn settings_move_root(&mut self, delta: isize) {
        if let Some(settings) = self.settings.as_mut() {
            settings.root_selection.move_by(9, delta);
        }
    }

    pub(super) fn settings_open_selected(&mut self) {
        let Some(settings) = self.settings.as_mut() else {
            return;
        };
        settings.searching = false;
        settings.choice_selection.clear_search();
        settings.page = match settings.root_selection.selected() {
            0 => SettingsPage::Profile,
            1 => SettingsPage::Provider,
            2 => SettingsPage::Model,
            3 => SettingsPage::Speed,
            4 => SettingsPage::Reasoning,
            5 => SettingsPage::Authentication,
            6 => SettingsPage::BaseUrl,
            7 => SettingsPage::Status,
            _ => SettingsPage::Review,
        };
        let selected = match settings.page {
            SettingsPage::Profile => settings
                .profiles
                .iter()
                .position(|value| value == &settings.active_profile)
                .unwrap_or(0),
            SettingsPage::Provider => provider_ids_with_current(&settings.profile.provider)
                .iter()
                .position(|value| value == &settings.profile.provider)
                .unwrap_or(0),
            SettingsPage::Model => provider_model_options(
                &settings.profile.provider,
                &settings.profile.model,
                &[],
            )
            .iter()
            .position(|value| value == &settings.profile.model)
            .unwrap_or(0),
            SettingsPage::Speed => ["fast", "balanced", "quality", "custom"]
                .iter()
                .position(|value| *value == settings.profile.speed)
                .unwrap_or(0),
            SettingsPage::Reasoning => ["low", "medium", "high", "maximum"]
                .iter()
                .position(|value| *value == settings.profile.reasoning)
                .unwrap_or(0),
            SettingsPage::Authentication => settings_auth_options(&settings.profile)
                .iter()
                .position(|value| value == &settings.profile.auth)
                .unwrap_or(0),
            SettingsPage::BaseUrl
            | SettingsPage::Status
            | SettingsPage::Review
            | SettingsPage::Root => 0,
        };
        settings.choice_selection.set_selected(selected, usize::MAX);
        if settings.page == SettingsPage::BaseUrl {
            settings.base_url_edit = settings.profile.base_url.clone().unwrap_or_default();
        }
    }

    pub(super) fn settings_back(&mut self) {
        if let Some(settings) = self.settings.as_mut() {
            settings.searching = false;
            settings.choice_selection.clear_search();
            settings.page = SettingsPage::Root;
        }
    }

    #[must_use]
    pub fn settings_searching(&self) -> bool {
        self.settings
            .as_ref()
            .is_some_and(|settings| settings.searching)
    }

    #[must_use]
    pub fn settings_search(&self) -> &str {
        self.settings
            .as_ref()
            .map(|settings| settings.choice_selection.search())
            .unwrap_or_default()
    }

    pub(super) fn settings_begin_search(&mut self) {
        if let Some(settings) = self.settings.as_mut()
            && matches!(
                settings.page,
                SettingsPage::Profile
                    | SettingsPage::Provider
                    | SettingsPage::Model
                    | SettingsPage::Speed
                    | SettingsPage::Reasoning
                    | SettingsPage::Authentication
            )
        {
            settings.searching = true;
        }
    }

    pub(super) fn settings_push_search(&mut self, character: char) {
        if let Some(settings) = self.settings.as_mut() {
            settings.choice_selection.push_search(character);
            normalize_settings_choice(settings);
        }
    }

    pub(super) fn settings_pop_search(&mut self) {
        if let Some(settings) = self.settings.as_mut() {
            settings.choice_selection.pop_search();
            normalize_settings_choice(settings);
        }
    }

    pub(super) fn settings_clear_search(&mut self) {
        if let Some(settings) = self.settings.as_mut() {
            settings.choice_selection.clear_search();
            settings.searching = false;
            normalize_settings_choice(settings);
        }
    }

    pub(super) fn settings_move_choice(&mut self, delta: isize) {
        let Some(settings) = self.settings.as_mut() else {
            return;
        };
        let choices = settings_choices(settings);
        let labels = choices
            .iter()
            .map(|choice| choice.label.as_str())
            .collect::<Vec<_>>();
        let filtered = settings.choice_selection.filtered_indices(&labels);
        settings
            .choice_selection
            .move_in_with(&filtered, delta, |index| choices[index].enabled);
    }

    #[must_use]
    pub fn settings_choices(&self) -> Vec<SettingsChoice> {
        self.settings
            .as_ref()
            .map(settings_choices)
            .unwrap_or_default()
    }

    #[must_use]
    pub fn settings_selected_choice(&self) -> usize {
        self.settings
            .as_ref()
            .map(|settings| settings.choice_selection.selected())
            .unwrap_or_default()
    }

    #[must_use]
    pub fn settings_doctor_checks(&self) -> Vec<ConfigDoctorCheck> {
        self.settings
            .as_ref()
            .map(|settings| settings.doctor.checks.clone())
            .unwrap_or_default()
    }

    #[must_use]
    pub fn settings_doctor_summary(&self) -> &str {
        self.settings
            .as_ref()
            .map_or("unavailable", |settings| settings.doctor.summary_label())
    }

    #[must_use]
    pub fn settings_review_lines(&self) -> Vec<String> {
        self.settings
            .as_ref()
            .map(|settings| {
                settings
                    .staged
                    .diff()
                    .into_iter()
                    .map(|entry| format!("{}: {} -> {}", entry.key, entry.before, entry.after))
                    .collect()
            })
            .unwrap_or_default()
    }

    #[must_use]
    pub fn settings_base_url_edit(&self) -> &str {
        self.settings
            .as_ref()
            .map(|settings| settings.base_url_edit.as_str())
            .unwrap_or_default()
    }

    pub(super) fn settings_insert_base_url(&mut self, text: &str) {
        if let Some(settings) = self.settings.as_mut() {
            settings.base_url_edit.push_str(text);
        }
    }

    pub(super) fn settings_delete_base_url_character(&mut self) {
        if let Some(settings) = self.settings.as_mut() {
            settings.base_url_edit.pop();
        }
    }

    pub(super) fn settings_commit_current(&mut self, repository: &Path) -> Result<AppAction, String> {
        let Some(settings) = self.settings.as_mut() else {
            return Ok(AppAction::None);
        };
        let current_revision = settings.catalog.revision().map_err(|error| error.to_string())?;
        if current_revision != settings.revision {
            return Err(format!(
                "configuration changed since settings opened (expected revision {}, current revision {current_revision}); reopen /settings",
                settings.revision
            ));
        }

        if settings.page == SettingsPage::Status {
            let selected = settings.choice_selection.selected();
            if let Some(check) = settings.doctor.checks.get(selected).cloned()
                && check.repair.is_some()
            {
                if let Some(change) = repair_config_check(&settings.catalog, &check)
                    .map_err(|error| error.to_string())?
                {
                    settings.revision = change.revision;
                    settings.last_apply_timing = Some(change.apply_timing);
                }
            }
            settings.doctor = diagnose_config_catalog(&settings.catalog)
                .map_err(|error| error.to_string())?;
            settings.choice_selection.set_selected(
                selected.min(settings.doctor.checks.len().saturating_sub(1)),
                settings.doctor.checks.len(),
            );
            return Ok(AppAction::Redraw);
        }

        if settings.page == SettingsPage::Review {
            if !settings.staged.is_dirty() {
                settings.page = SettingsPage::Root;
                return Ok(AppAction::Redraw);
            }
            validate_settings_candidate(repository, settings.staged.candidate())?;
            let change = settings
                .staged
                .clone()
                .commit(
                    &settings.catalog,
                    ConfigurationChangeOrigin::Tui,
                    ConfigurationApplyTiming::NextSession,
                )
                .map_err(|error| error.to_string())?;
            settings.revision = change.revision;
            settings.last_apply_timing = Some(change.apply_timing);
            return Ok(AppAction::Redraw);
        }

        if settings.page == SettingsPage::Profile {
            let name = selected_settings_choice(settings)
                .map(|choice| choice.label)
                .ok_or_else(|| "no matching provider profile is selected".to_owned())?;
            if name == settings.active_profile {
                settings.page = SettingsPage::Root;
                return Ok(AppAction::Redraw);
            }
            return Ok(AppAction::Command(SlashCommand::Config(
                ConfigCommand::UseProfile { name },
            )));
        }

        if settings.page == SettingsPage::Provider {
            let provider = selected_settings_choice(settings)
                .map(|choice| choice.label)
                .ok_or_else(|| "no matching available provider route is selected".to_owned())?;
            let entry = provider_catalog_entry(&provider)
                .ok_or_else(|| format!("provider route `{provider}` is not in the catalog"))?;
            if entry.connection == settings.profile.connection
                && (entry.profile_provider == settings.profile.provider
                    || entry.id == settings.profile.provider)
            {
                settings.page = SettingsPage::Root;
                return Ok(AppAction::Redraw);
            }
            let mut candidate = settings.profile.clone();
            apply_provider_defaults(entry, &mut candidate);
            candidate.configured = true;
            validate_settings_candidate(repository, &candidate)?;
            settings
                .staged
                .replace(candidate.clone())
                .map_err(|error| error.to_string())?;
            settings.profile = candidate;
            settings.page = SettingsPage::Root;
            return Ok(AppAction::Redraw);
        }

        if settings.page == SettingsPage::BaseUrl {
            let entry = provider_catalog_entry(&settings.profile.provider);
            if entry.is_some_and(|entry| !entry.custom_values) {
                return Err("base URL is managed by the selected provider route".to_owned());
            }
            let value = settings.base_url_edit.trim();
            let mut candidate = settings.profile.clone();
            if value.is_empty() {
                candidate
                    .unset_value("base_url")
                    .map_err(|error| error.to_string())?;
            } else {
                candidate
                    .set_value("base_url", value)
                    .map_err(|error| error.to_string())?;
            }
            validate_settings_candidate(repository, &candidate)?;
            settings
                .staged
                .replace(candidate.clone())
                .map_err(|error| error.to_string())?;
            settings.profile = candidate;
            settings.page = SettingsPage::Root;
            return Ok(AppAction::Redraw);
        }

        let value = selected_settings_choice(settings)
            .map(|choice| choice.label)
            .ok_or_else(|| "no matching settings value is selected".to_owned())?;
        let key = match settings.page {
            SettingsPage::Model => "model",
            SettingsPage::Speed => "speed",
            SettingsPage::Reasoning => "reasoning",
            SettingsPage::Authentication => "auth",
            SettingsPage::Root
            | SettingsPage::Profile
            | SettingsPage::Provider
            | SettingsPage::BaseUrl
            | SettingsPage::Status
            | SettingsPage::Review => return Ok(AppAction::Redraw),
        };
        let mut candidate = settings.profile.clone();
        candidate
            .set_value(key, &value)
            .map_err(|error| error.to_string())?;
        validate_settings_candidate(repository, &candidate)?;
        settings
            .staged
            .replace(candidate.clone())
            .map_err(|error| error.to_string())?;
        settings.profile = candidate;
        settings.page = SettingsPage::Root;
        Ok(AppAction::Redraw)
    }
}

fn settings_auth_options(profile: &ProviderProfile) -> Vec<String> {
    provider_catalog_entry(&profile.provider).map_or_else(
        || {
            vec![
                "api-key".to_owned(),
                "oauth".to_owned(),
                "existing".to_owned(),
                "none".to_owned(),
            ]
        },
        |entry| {
            entry
                .auth_methods
                .iter()
                .map(|method| (*method).to_owned())
                .collect()
        },
    )
}

fn settings_choices(settings: &SettingsState) -> Vec<SettingsChoice> {
    match settings.page {
        SettingsPage::Root | SettingsPage::BaseUrl | SettingsPage::Review => Vec::new(),
        SettingsPage::Status => settings
            .doctor
            .checks
            .iter()
            .map(|check| SettingsChoice {
                label: check.name.clone(),
                description: check.detail.clone(),
                enabled: true,
            })
            .collect(),
        SettingsPage::Profile => settings
            .profiles
            .iter()
            .map(|name| SettingsChoice {
                label: name.clone(),
                description: if name == &settings.active_profile {
                    "active profile".to_owned()
                } else {
                    "saved provider profile".to_owned()
                },
                enabled: true,
            })
            .collect(),
        SettingsPage::Provider => provider_ids_with_current(&settings.profile.provider)
            .into_iter()
            .map(|provider| {
                let entry = provider_catalog_entry(&provider);
                SettingsChoice {
                    description: entry.map_or_else(
                        || "configured custom provider".to_owned(),
                        |entry| entry.description.to_owned(),
                    ),
                    enabled: entry.is_none_or(|entry| entry.disabled_reason.is_none()),
                    label: provider,
                }
            })
            .collect(),
        SettingsPage::Model => provider_model_options(
            &settings.profile.provider,
            &settings.profile.model,
            &[],
        )
        .into_iter()
        .map(|model| SettingsChoice {
            label: model,
            description: "model".to_owned(),
            enabled: true,
        })
        .collect(),
        SettingsPage::Speed => ["fast", "balanced", "quality", "custom"]
            .into_iter()
            .map(|value| SettingsChoice {
                label: value.to_owned(),
                description: "speed preference".to_owned(),
                enabled: true,
            })
            .collect(),
        SettingsPage::Reasoning => ["low", "medium", "high", "maximum"]
            .into_iter()
            .map(|value| SettingsChoice {
                label: value.to_owned(),
                description: "reasoning preference".to_owned(),
                enabled: true,
            })
            .collect(),
        SettingsPage::Authentication => settings_auth_options(&settings.profile)
            .into_iter()
            .map(|value| SettingsChoice {
                label: value,
                description: "credential material remains external".to_owned(),
                enabled: true,
            })
            .collect(),
    }
}

fn selected_settings_choice(settings: &SettingsState) -> Option<SettingsChoice> {
    let choices = settings_choices(settings);
    let labels = choices
        .iter()
        .map(|choice| choice.label.as_str())
        .collect::<Vec<_>>();
    let selected = settings.choice_selection.selected();
    settings
        .choice_selection
        .filtered_indices(&labels)
        .contains(&selected)
        .then(|| choices.get(selected))
        .flatten()
        .filter(|choice| choice.enabled)
        .cloned()
}

fn normalize_settings_choice(settings: &mut SettingsState) {
    let choices = settings_choices(settings);
    let labels = choices
        .iter()
        .map(|choice| choice.label.as_str())
        .collect::<Vec<_>>();
    let filtered = settings.choice_selection.filtered_indices(&labels);
    if let Some(selected) = filtered
        .iter()
        .copied()
        .find(|index| choices[*index].enabled)
    {
        if !filtered.contains(&settings.choice_selection.selected())
            || !choices[settings.choice_selection.selected()].enabled
        {
            settings
                .choice_selection
                .set_selected(selected, choices.len());
        }
    }
}

fn validate_settings_candidate(repository: &Path, profile: &ProviderProfile) -> Result<(), String> {
    let project = repository.join(".medusa/config.toml");
    let project = project.exists().then_some(project);
    Config::load_layers_with_provider_profile(
        profile,
        None,
        project.as_deref(),
        &BTreeMap::new(),
        &BTreeMap::new(),
    )
    .map(|_| ())
    .map_err(|error| error.to_string())
}

fn effort_from_label(label: Option<&str>) -> Effort {
    match label.unwrap_or_default() {
        "effort:low" => Effort::Low,
        "effort:medium" => Effort::Medium,
        "effort:auto" => Effort::Auto,
        _ => Effort::High,
    }
}

fn cycle_index(current: usize, length: usize, delta: isize) -> usize {
    if length == 0 {
        return 0;
    }
    (current as isize + delta).rem_euclid(length as isize) as usize
}

#[cfg(test)]
mod settings_tests {
    use super::*;

    fn catalog_at(root: &Path) -> ProviderProfileCatalog {
        let catalog = ProviderProfileCatalog::at(root.join("config"));
        catalog
            .active_store()
            .expect("active store")
            .save(&ProviderProfile {
                configured: true,
                ..ProviderProfile::default()
            })
            .expect("save profile");
        catalog
    }

    #[test]
    fn settings_render_current_catalog_values_and_preserve_nested_selection() {
        let directory = tempfile::tempdir().expect("tempdir");
        let catalog = catalog_at(directory.path());
        let mut modal = ModelModal::new_settings_with_catalog(
            Some("minimax / MiniMax-M3"),
            Some("effort:high"),
            false,
            catalog,
        )
        .expect("settings");

        let rows = modal.settings_root_rows();
        assert!(rows.contains(&("Profile".to_owned(), "default".to_owned())));
        assert!(rows.contains(&("Provider".to_owned(), "minimax".to_owned())));
        assert!(rows.contains(&("Model".to_owned(), "MiniMax-M3".to_owned())));
        assert!(rows.contains(&("Speed".to_owned(), "balanced".to_owned())));
        assert!(rows.contains(&("Reasoning".to_owned(), "medium".to_owned())));
        assert!(rows.contains(&("Authentication".to_owned(), "api-key".to_owned())));

        modal.settings_move_root(1);
        assert_eq!(modal.settings_root_selected(), 1);
        modal.settings_open_selected();
        assert_eq!(modal.settings_page(), Some(SettingsPage::Provider));
        modal.settings_back();
        assert_eq!(modal.settings_page(), Some(SettingsPage::Root));
        assert_eq!(modal.settings_root_selected(), 1);
    }

    #[test]
    fn navigating_and_cancelling_settings_does_not_write_configuration() {
        let directory = tempfile::tempdir().expect("tempdir");
        let catalog = catalog_at(directory.path());
        let revision = catalog.revision().expect("revision");
        let mut modal = ModelModal::new_settings_with_catalog(None, None, false, catalog.clone())
            .expect("settings");
        modal.settings_move_root(2);
        modal.settings_open_selected();
        modal.settings_move_choice(1);
        modal.settings_back();
        assert_eq!(catalog.revision().expect("revision"), revision);
    }

    #[test]
    fn stale_settings_revision_is_rejected_before_mutation() {
        let directory = tempfile::tempdir().expect("tempdir");
        let catalog = catalog_at(directory.path());
        let snapshot = catalog.snapshot().expect("snapshot");
        let mut modal = ModelModal::new_settings_with_catalog(None, None, false, catalog.clone())
            .expect("settings");

        let mut external = snapshot.profile;
        external
            .set_value("model", "MiniMax-M2.7")
            .expect("external model");
        catalog
            .save_active_profile(
                &external,
                snapshot.revision,
                ConfigurationChangeOrigin::Cli,
                ["model".to_owned()],
                ConfigurationApplyTiming::Immediate,
            )
            .expect("external save");

        modal.settings_move_root(1);
        modal.settings_open_selected();
        let error = modal
            .settings_commit_current(directory.path())
            .expect_err("stale settings must fail");
        assert!(error.contains("configuration changed since settings opened"));
        assert_eq!(catalog.snapshot().expect("snapshot").profile, external);
    }

    #[test]
    fn current_provider_route_is_a_noop_and_preserves_profile() {
        let directory = tempfile::tempdir().expect("tempdir");
        let catalog = catalog_at(directory.path());
        let mut current = catalog.snapshot().expect("snapshot").profile;
        current
            .set_value("model", "MiniMax-M2.7")
            .expect("custom current model");
        catalog
            .active_store()
            .expect("store")
            .save(&current)
            .expect("save current profile");
        let revision = catalog.revision().expect("revision");
        let mut modal = ModelModal::new_settings_with_catalog(None, None, false, catalog.clone())
            .expect("settings");
        modal.settings_move_root(1);
        modal.settings_open_selected();

        assert_eq!(
            modal
                .settings_commit_current(directory.path())
                .expect("current route"),
            AppAction::Redraw
        );
        assert_eq!(catalog.revision().expect("revision"), revision);
        assert_eq!(catalog.snapshot().expect("snapshot").profile, current);
    }

    #[test]
    fn zero_match_filter_cannot_apply_hidden_settings_choice() {
        let directory = tempfile::tempdir().expect("tempdir");
        let catalog = catalog_at(directory.path());
        let revision = catalog.revision().expect("revision");
        let mut modal = ModelModal::new_settings_with_catalog(None, None, false, catalog.clone())
            .expect("settings");
        modal.settings_move_root(2);
        modal.settings_open_selected();
        modal.settings_begin_search();
        for character in "definitely-no-match".chars() {
            modal.settings_push_search(character);
        }

        let error = modal
            .settings_commit_current(directory.path())
            .expect_err("hidden selection must not apply");
        assert!(error.contains("no matching settings value"));
        assert_eq!(catalog.revision().expect("revision"), revision);
    }

    #[test]
    fn provider_route_change_stages_then_records_tui_origin_and_next_session_timing_on_apply() {
        let directory = tempfile::tempdir().expect("tempdir");
        let catalog = catalog_at(directory.path());
        let revision = catalog.revision().expect("revision");
        let original = catalog.snapshot().expect("snapshot").profile;
        let mut modal = ModelModal::new_settings_with_catalog(None, None, false, catalog.clone())
            .expect("settings");
        modal.settings_move_root(1);
        modal.settings_open_selected();
        modal.settings_move_choice(1);
        assert_eq!(
            modal.settings_choices()[modal.settings_selected_choice()].label,
            "anthropic"
        );
        assert_eq!(
            modal
                .settings_commit_current(directory.path())
                .expect("stage provider change"),
            AppAction::Redraw
        );

        assert_eq!(catalog.revision().expect("revision"), revision);
        assert_eq!(catalog.snapshot().expect("snapshot").profile, original);
        assert!(
            modal
                .settings_review_lines()
                .iter()
                .any(|line| line.starts_with("provider:"))
        );

        modal.settings_move_root(7);
        modal.settings_open_selected();
        assert_eq!(
            modal
                .settings_commit_current(directory.path())
                .expect("apply staged provider change"),
            AppAction::Redraw
        );

        let change = catalog
            .last_change()
            .expect("last change")
            .expect("change");
        assert_eq!(change.origin, ConfigurationChangeOrigin::Tui);
        assert_eq!(change.apply_timing, ConfigurationApplyTiming::NextSession);
        let profile = catalog.snapshot().expect("snapshot").profile;
        assert_eq!(profile.provider, "anthropic");
        assert!(profile.configured);
        let effective = Config::load_layers_with_provider_profile(
            &profile,
            None,
            None,
            &BTreeMap::new(),
            &BTreeMap::new(),
        )
        .expect("effective config");
        assert_eq!(effective.model.provider, "anthropic");
        assert_eq!(effective.model.name, "claude-sonnet-4-6");
    }
}
