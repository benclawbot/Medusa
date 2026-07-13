use std::{io, path::PathBuf, sync::Arc, time::Instant};

use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};

use crate::{
    clipboard::{
        ClipboardContent, ClipboardError, ClipboardService, FileAttachment, PromptAttachment,
        PromptDraft,
    },
    commands::{
        Effort, ModelCommand, ModelConfiguration, SlashCommand, command_suggestions,
        complete_first_command, parse_slash_command,
    },
    draft_store::DraftStore,
    input::{ComposerAction, ComposerState},
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TranscriptEntry {
    User(PromptDraft),
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
    selected_options: Vec<usize>,
    active_question: usize,
    reviewing: bool,
}

impl QuestionModal {
    pub fn new(questions: Vec<QuestionPrompt>) -> Self {
        let count = questions.len();
        Self {
            questions,
            answers: vec![QuestionAnswer::default(); count],
            selected_options: vec![0; count],
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
        self.selected_options
            .get(self.active_question)
            .copied()
            .unwrap_or_default()
    }

    #[must_use]
    pub fn active_custom_answer(&self) -> &str {
        self.answers
            .get(self.active_question)
            .map(|answer| answer.custom_answer.as_str())
            .unwrap_or_default()
    }

    fn move_selection(&mut self, delta: isize) {
        let active_question = self.active_question;
        let option_count = self
            .questions
            .get(active_question)
            .map_or(0, |prompt| prompt.options.len());
        if let Some(selected) = self.selected_options.get_mut(active_question)
            && option_count > 0
        {
            *selected = cycle_index(*selected, option_count, delta);
        }
    }

    fn move_question(&mut self, delta: isize) {
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

    fn advance_or_review(&mut self) {
        if self.active_question.saturating_add(1) < self.questions.len() {
            self.active_question = self.active_question.saturating_add(1);
        } else {
            self.reviewing = true;
        }
    }

    fn toggle_current_option(&mut self) {
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

    fn select_current_answer(&mut self) -> bool {
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

    fn insert_answer(&mut self, text: &str) {
        if let Some(answer) = self.answers.get_mut(self.active_question) {
            answer.custom_answer.push_str(text);
        }
    }

    fn delete_answer_character(&mut self) {
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

    fn answer_bundle(&self) -> Option<String> {
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelModal {
    provider_index: usize,
    model_index: usize,
    model_options: Vec<String>,
    effort: Effort,
    focus: ModelModalFocus,
    api_key: String,
    has_existing_key: bool,
}

impl ModelModal {
    const PROVIDERS: [&str; 3] = ["minimax", "anthropic", "anthropic-compatible"];

    fn new(model_label: Option<&str>, effort_label: Option<&str>, has_existing_key: bool) -> Self {
        let (provider, current_model) = model_label
            .and_then(|label| label.split_once(" / "))
            .unwrap_or(("minimax", "MiniMax-M3"));
        let provider_index = Self::PROVIDERS
            .iter()
            .position(|candidate| *candidate == provider)
            .unwrap_or(0);
        let models = model_options_for(Self::PROVIDERS[provider_index], current_model);
        let model_index = models
            .iter()
            .position(|candidate| candidate == current_model)
            .unwrap_or(0);
        Self {
            provider_index,
            model_index,
            model_options: models,
            effort: effort_from_label(effort_label),
            focus: ModelModalFocus::Model,
            api_key: String::new(),
            has_existing_key,
        }
    }

    #[must_use]
    pub fn provider(&self) -> &str {
        Self::PROVIDERS[self.provider_index]
    }

    #[must_use]
    pub fn model_options(&self) -> Vec<String> {
        self.model_options.clone()
    }

    #[must_use]
    pub fn selected_model(&self) -> String {
        self.model_options
            .get(self.model_index)
            .cloned()
            .unwrap_or_else(|| "MiniMax-M3".to_owned())
    }

    #[must_use]
    pub const fn selected_model_index(&self) -> usize {
        self.model_index
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

    fn cycle_focus(&mut self) {
        self.focus = match self.focus {
            ModelModalFocus::Provider => ModelModalFocus::Model,
            ModelModalFocus::Model => ModelModalFocus::Effort,
            ModelModalFocus::Effort => ModelModalFocus::ApiKey,
            ModelModalFocus::ApiKey => ModelModalFocus::Apply,
            ModelModalFocus::Apply => ModelModalFocus::Provider,
        };
    }

    fn cycle_focus_back(&mut self) {
        self.focus = match self.focus {
            ModelModalFocus::Provider => ModelModalFocus::Apply,
            ModelModalFocus::Model => ModelModalFocus::Provider,
            ModelModalFocus::Effort => ModelModalFocus::Model,
            ModelModalFocus::ApiKey => ModelModalFocus::Effort,
            ModelModalFocus::Apply => ModelModalFocus::ApiKey,
        };
    }

    fn focus_api_key(&mut self) {
        self.focus = ModelModalFocus::ApiKey;
    }

    fn move_selection(&mut self, delta: isize) {
        match self.focus {
            ModelModalFocus::Provider => {
                self.provider_index =
                    cycle_index(self.provider_index, Self::PROVIDERS.len(), delta);
                self.model_options = model_options_for(self.provider(), "");
                self.model_index = 0;
            }
            ModelModalFocus::Model => {
                self.model_index = cycle_index(self.model_index, self.model_options.len(), delta);
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

    fn insert_key_text(&mut self, text: &str) {
        self.api_key
            .extend(text.chars().filter(|character| !character.is_whitespace()));
    }

    fn delete_key_character(&mut self) {
        self.api_key.pop();
    }
}

fn model_options_for(provider: &str, current_model: &str) -> Vec<String> {
    let mut models = match provider {
        "minimax" => vec!["MiniMax-M3".to_owned(), "MiniMax-M2.7".to_owned()],
        "anthropic" => vec![
            "claude-opus-4-6".to_owned(),
            "claude-sonnet-4-6".to_owned(),
            "claude-haiku-4-5".to_owned(),
        ],
        _ => vec!["custom-model".to_owned()],
    };
    if !current_model.is_empty() && !models.iter().any(|model| model == current_model) {
        models.insert(0, current_model.to_owned());
    }
    models
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
    (current as isize + delta).rem_euclid(length as isize) as usize
}

pub struct AppState {
    pub composer: ComposerState,
    pub transcript: Vec<TranscriptEntry>,
    pub plan: Option<TranscriptPlan>,
    pub status: String,
    pub token_count: u64,
    pub active_turn: u32,
    pub command_selection: usize,
    pub model_label: Option<String>,
    pub effort_label: Option<String>,
    pub plan_mode: bool,
    pub task_list_visible: bool,
    pub spinner_frame: u8,
    welcome_visible: bool,
    credential_configured: bool,
    model_modal: Option<ModelModal>,
    question_modal: Option<QuestionModal>,
    run_started_at: Option<Instant>,
    draft_store: DraftStore,
    draft_key: String,
    clipboard: Arc<dyn ClipboardService>,
}

impl AppState {
    pub fn new(
        repository: PathBuf,
        draft_key: impl Into<String>,
        initial_text: impl Into<String>,
        clipboard: Arc<dyn ClipboardService>,
    ) -> io::Result<Self> {
        let draft_key = draft_key.into();
        let draft_store = DraftStore::for_repo(&repository);
        let composer = match draft_store.load(&draft_key)? {
            Some(draft) => ComposerState {
                cursor: draft.text.len(),
                draft,
            },
            None => ComposerState::new(initial_text),
        };
        Ok(Self {
            composer,
            transcript: Vec::new(),
            plan: None,
            status: "ready".to_owned(),
            token_count: 0,
            active_turn: 0,
            command_selection: 0,
            model_label: None,
            effort_label: None,
            plan_mode: false,
            task_list_visible: true,
            spinner_frame: 0,
            welcome_visible: true,
            credential_configured: false,
            model_modal: None,
            question_modal: None,
            run_started_at: None,
            draft_store,
            draft_key,
            clipboard,
        })
    }

    pub fn handle_event(&mut self, event: Event) -> Result<AppAction, AppError> {
        self.dismiss_welcome_for_event(&event);
        if self.question_modal.is_some() {
            return self.handle_question_modal_event(event);
        }
        if self.model_modal.is_some() {
            return self.handle_model_modal_event(event);
        }
        if let Event::Key(key) = &event
            && key.kind == KeyEventKind::Press
        {
            if key.code == KeyCode::Esc {
                return Ok(AppAction::Quit);
            }
            if key.code == KeyCode::Char('t') && key.modifiers.contains(KeyModifiers::CONTROL) {
                self.task_list_visible = !self.task_list_visible;
                return Ok(AppAction::Redraw);
            }
            if key.code == KeyCode::Char('v') && key.modifiers.contains(KeyModifiers::CONTROL) {
                self.paste_from_clipboard()?;
                self.persist_draft()?;
                return Ok(AppAction::Redraw);
            }
        }

        match self.composer.handle_event(event)? {
            ComposerAction::None => Ok(AppAction::None),
            ComposerAction::Changed => {
                self.command_selection = 0;
                self.persist_draft()?;
                Ok(AppAction::Redraw)
            }
            ComposerAction::Interrupt => Ok(AppAction::Interrupt),
            ComposerAction::CommandPrevious => self.select_command(-1),
            ComposerAction::CommandNext => self.select_command(1),
            ComposerAction::CompleteCommand => self.complete_command(),
            ComposerAction::Submit => {
                let submitted = self.composer.draft.clone();
                if submitted.attachments.is_empty() && submitted.text.trim() == "/" {
                    self.status = "choose a command".to_owned();
                    return Ok(AppAction::Redraw);
                }
                self.composer = ComposerState::new("");
                self.draft_store.delete(&self.draft_key)?;
                self.command_selection = 0;
                if submitted.attachments.is_empty() {
                    match parse_slash_command(&submitted.text) {
                        Ok(Some(command)) => {
                            if matches!(command, SlashCommand::Model(ModelCommand::Show)) {
                                self.model_modal = Some(ModelModal::new(
                                    self.model_label.as_deref(),
                                    self.effort_label.as_deref(),
                                    self.credential_configured,
                                ));
                                self.status = "model configuration".to_owned();
                                return Ok(AppAction::Redraw);
                            }
                            if let SlashCommand::Plan { task: Some(task) } = &command {
                                self.transcript.push(TranscriptEntry::User(PromptDraft {
                                    text: task.clone(),
                                    ..PromptDraft::default()
                                }));
                                self.plan = None;
                            }
                            self.status = "command submitted".to_owned();
                            return Ok(AppAction::Command(command));
                        }
                        Ok(None) => {}
                        Err(error) => {
                            self.transcript
                                .push(TranscriptEntry::System(format!("error: {error}")));
                            self.status = "command rejected".to_owned();
                            return Ok(AppAction::Redraw);
                        }
                    }
                }
                self.transcript
                    .push(TranscriptEntry::User(submitted.clone()));
                self.plan = None;
                self.status = "prompt submitted".to_owned();
                Ok(AppAction::Submit(submitted))
            }
        }
    }

    #[must_use]
    pub fn welcome_visible(&self) -> bool {
        self.welcome_visible
    }

    pub fn dismiss_welcome_for_event(&mut self, event: &Event) {
        if matches!(event, Event::Key(key) if key.kind != KeyEventKind::Release)
            || matches!(event, Event::Paste(_) | Event::Mouse(_))
        {
            self.welcome_visible = false;
        }
    }

    pub fn clear_for_new_session(&mut self) {
        self.transcript.clear();
        self.plan = None;
        self.token_count = 0;
        self.active_turn = 0;
        self.status = "new session".to_owned();
        self.plan_mode = false;
        self.task_list_visible = true;
        self.question_modal = None;
    }

    pub fn compact_transcript(&mut self, message: String) {
        const TRANSCRIPT_TAIL: usize = 8;
        if self.transcript.len() > TRANSCRIPT_TAIL {
            self.transcript = self
                .transcript
                .split_off(self.transcript.len().saturating_sub(TRANSCRIPT_TAIL));
        }
        self.transcript.insert(0, TranscriptEntry::System(message));
        self.status = "context compacted".to_owned();
    }

    pub fn set_runtime_settings(
        &mut self,
        model: String,
        effort: String,
        plan_mode: bool,
        credential_configured: bool,
    ) {
        self.model_label = Some(model);
        self.effort_label = Some(effort);
        self.plan_mode = plan_mode;
        self.credential_configured = credential_configured;
    }

    pub fn set_plan(&mut self, plan: TranscriptPlan) {
        self.plan = Some(plan);
        self.task_list_visible = true;
    }

    pub fn open_question(&mut self, questions: Vec<QuestionPrompt>) {
        self.question_modal = Some(QuestionModal::new(questions));
        self.status = "waiting for your answer".to_owned();
        self.finish_run();
    }

    fn select_command(&mut self, direction: isize) -> Result<AppAction, AppError> {
        let suggestions = command_suggestions(&self.composer.draft.text);
        if suggestions.is_empty() {
            return Ok(AppAction::None);
        }
        let length = suggestions.len() as isize;
        self.command_selection =
            (self.command_selection as isize + direction).rem_euclid(length) as usize;
        Ok(AppAction::Redraw)
    }

    fn complete_command(&mut self) -> Result<AppAction, AppError> {
        let suggestions = command_suggestions(&self.composer.draft.text);
        let Some(selected) = suggestions.get(self.command_selection) else {
            return Ok(AppAction::None);
        };
        let completed = if self.command_selection == 0 {
            complete_first_command(&self.composer.draft.text)
        } else {
            Some(format!("/{} ", selected.name))
        };
        if let Some(completed) = completed {
            self.composer = ComposerState::new(completed);
            self.command_selection = 0;
            self.persist_draft()?;
            return Ok(AppAction::Redraw);
        }
        Ok(AppAction::None)
    }

    fn handle_model_modal_event(&mut self, event: Event) -> Result<AppAction, AppError> {
        match event {
            Event::Paste(text) => {
                if let Some(modal) = self.model_modal.as_mut() {
                    modal.focus_api_key();
                    modal.insert_key_text(&text);
                }
                Ok(AppAction::Redraw)
            }
            Event::Key(key) if key.kind != KeyEventKind::Release => match key.code {
                KeyCode::Esc => {
                    self.model_modal = None;
                    self.status = "model configuration cancelled".to_owned();
                    Ok(AppAction::Redraw)
                }
                KeyCode::Enter => {
                    let submit = self
                        .model_modal
                        .as_ref()
                        .is_some_and(|modal| modal.focus == ModelModalFocus::Apply);
                    if submit {
                        let configuration = self
                            .model_modal
                            .take()
                            .expect("model modal exists")
                            .configuration();
                        Ok(AppAction::ConfigureModel(configuration))
                    } else {
                        self.model_modal
                            .as_mut()
                            .expect("model modal exists")
                            .cycle_focus();
                        Ok(AppAction::Redraw)
                    }
                }
                KeyCode::BackTab => {
                    self.model_modal
                        .as_mut()
                        .expect("model modal exists")
                        .cycle_focus_back();
                    Ok(AppAction::Redraw)
                }
                KeyCode::Tab if key.modifiers.contains(KeyModifiers::SHIFT) => {
                    self.model_modal
                        .as_mut()
                        .expect("model modal exists")
                        .cycle_focus_back();
                    Ok(AppAction::Redraw)
                }
                KeyCode::Tab => {
                    self.model_modal
                        .as_mut()
                        .expect("model modal exists")
                        .cycle_focus();
                    Ok(AppAction::Redraw)
                }
                KeyCode::Up | KeyCode::Left => {
                    self.model_modal
                        .as_mut()
                        .expect("model modal exists")
                        .move_selection(-1);
                    Ok(AppAction::Redraw)
                }
                KeyCode::Down | KeyCode::Right => {
                    self.model_modal
                        .as_mut()
                        .expect("model modal exists")
                        .move_selection(1);
                    Ok(AppAction::Redraw)
                }
                KeyCode::Backspace => {
                    if let Some(modal) = self.model_modal.as_mut()
                        && modal.focus == ModelModalFocus::ApiKey
                    {
                        modal.delete_key_character();
                    }
                    Ok(AppAction::Redraw)
                }
                KeyCode::Char('v') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    let clipboard = self.clipboard.read()?;
                    if let ClipboardContent::Text(text) = clipboard
                        && let Some(modal) = self.model_modal.as_mut()
                    {
                        modal.focus_api_key();
                        modal.insert_key_text(&text);
                    }
                    Ok(AppAction::Redraw)
                }
                KeyCode::Char(character)
                    if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT =>
                {
                    if let Some(modal) = self.model_modal.as_mut()
                        && modal.focus == ModelModalFocus::ApiKey
                    {
                        modal.insert_key_text(&character.to_string());
                    }
                    Ok(AppAction::Redraw)
                }
                _ => Ok(AppAction::None),
            },
            _ => Ok(AppAction::None),
        }
    }

    fn handle_question_modal_event(&mut self, event: Event) -> Result<AppAction, AppError> {
        match event {
            Event::Paste(text) => {
                self.question_modal
                    .as_mut()
                    .expect("question modal exists")
                    .insert_answer(&text);
                Ok(AppAction::Redraw)
            }
            Event::Key(key) if key.kind != KeyEventKind::Release => match key.code {
                KeyCode::Esc => {
                    if let Some(modal) = self.question_modal.as_mut()
                        && modal.is_reviewing()
                    {
                        modal.move_question(-1);
                    }
                    self.status = "waiting for your answers".to_owned();
                    Ok(AppAction::Redraw)
                }
                KeyCode::BackTab => {
                    self.question_modal
                        .as_mut()
                        .expect("question modal exists")
                        .move_question(-1);
                    Ok(AppAction::Redraw)
                }
                KeyCode::Tab if key.modifiers.contains(KeyModifiers::SHIFT) => {
                    self.question_modal
                        .as_mut()
                        .expect("question modal exists")
                        .move_question(-1);
                    Ok(AppAction::Redraw)
                }
                KeyCode::Tab => {
                    self.question_modal
                        .as_mut()
                        .expect("question modal exists")
                        .move_question(1);
                    Ok(AppAction::Redraw)
                }
                KeyCode::Up | KeyCode::Left => {
                    if self
                        .question_modal
                        .as_ref()
                        .is_some_and(QuestionModal::is_reviewing)
                    {
                        return Ok(AppAction::Redraw);
                    }
                    self.question_modal
                        .as_mut()
                        .expect("question modal exists")
                        .move_selection(-1);
                    Ok(AppAction::Redraw)
                }
                KeyCode::Down | KeyCode::Right => {
                    if self
                        .question_modal
                        .as_ref()
                        .is_some_and(QuestionModal::is_reviewing)
                    {
                        return Ok(AppAction::Redraw);
                    }
                    self.question_modal
                        .as_mut()
                        .expect("question modal exists")
                        .move_selection(1);
                    Ok(AppAction::Redraw)
                }
                KeyCode::Backspace => {
                    if self
                        .question_modal
                        .as_ref()
                        .is_some_and(|modal| !modal.is_reviewing())
                    {
                        self.question_modal
                            .as_mut()
                            .expect("question modal exists")
                            .delete_answer_character();
                    }
                    Ok(AppAction::Redraw)
                }
                KeyCode::Char('v') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    if self
                        .question_modal
                        .as_ref()
                        .is_some_and(|modal| !modal.is_reviewing())
                        && let ClipboardContent::Text(text) = self.clipboard.read()?
                    {
                        self.question_modal
                            .as_mut()
                            .expect("question modal exists")
                            .insert_answer(&text);
                    }
                    Ok(AppAction::Redraw)
                }
                KeyCode::Char(' ') if key.modifiers.is_empty() => {
                    if self
                        .question_modal
                        .as_ref()
                        .is_some_and(|modal| !modal.is_reviewing())
                    {
                        self.question_modal
                            .as_mut()
                            .expect("question modal exists")
                            .toggle_current_option();
                    }
                    Ok(AppAction::Redraw)
                }
                KeyCode::Char(character)
                    if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT =>
                {
                    if self
                        .question_modal
                        .as_ref()
                        .is_some_and(|modal| !modal.is_reviewing())
                    {
                        self.question_modal
                            .as_mut()
                            .expect("question modal exists")
                            .insert_answer(&character.to_string());
                    }
                    Ok(AppAction::Redraw)
                }
                KeyCode::Enter => {
                    let modal = self.question_modal.as_mut().expect("question modal exists");
                    if modal.is_reviewing() {
                        let Some(answer) = modal.answer_bundle() else {
                            self.status = "answer every question before confirming".to_owned();
                            return Ok(AppAction::Redraw);
                        };
                        self.question_modal = None;
                        self.transcript.push(TranscriptEntry::User(PromptDraft {
                            text: answer.clone(),
                            ..PromptDraft::default()
                        }));
                        self.status = "answers confirmed".to_owned();
                        return Ok(AppAction::AnswerQuestion(answer));
                    }
                    if !modal.select_current_answer() {
                        self.status = "choose or type an answer".to_owned();
                        return Ok(AppAction::Redraw);
                    }
                    modal.advance_or_review();
                    Ok(AppAction::Redraw)
                }
                _ => Ok(AppAction::None),
            },
            _ => Ok(AppAction::None),
        }
    }

    pub fn paste_from_clipboard(&mut self) -> Result<(), AppError> {
        match self.clipboard.read()? {
            ClipboardContent::Empty => {
                self.status = "clipboard is empty".to_owned();
            }
            ClipboardContent::Text(text) => {
                self.composer
                    .draft
                    .insert_pasted_text(self.composer.cursor, &text)?;
                self.composer.cursor = self.composer.draft.text.len();
                self.status = format!("pasted {} bytes of text", text.len());
            }
            ClipboardContent::Image(image) => {
                let width = image.width;
                let height = image.height;
                self.composer.draft.add_image(image)?;
                self.status = format!("attached screenshot {width}×{height}");
            }
            ClipboardContent::Files(paths) => {
                let mut added = 0_usize;
                for path in paths {
                    let metadata = std::fs::metadata(&path)?;
                    if metadata.is_file() {
                        self.composer.draft.attachments.push(PromptAttachment::File(
                            FileAttachment {
                                path,
                                byte_len: usize::try_from(metadata.len()).unwrap_or(usize::MAX),
                            },
                        ));
                        added = added.saturating_add(1);
                    }
                }
                self.composer.draft.revision = self.composer.draft.revision.saturating_add(1);
                self.status = format!("attached {added} clipboard file(s)");
            }
        }
        Ok(())
    }

    pub fn begin_run(&mut self) {
        self.status = "Working".to_owned();
        self.token_count = 0;
        self.active_turn = 0;
        self.spinner_frame = 0;
        self.run_started_at = Some(Instant::now());
    }

    pub fn finish_run(&mut self) {
        self.run_started_at = None;
    }

    pub fn tick(&mut self) -> bool {
        if self.is_running() {
            self.spinner_frame = self.spinner_frame.wrapping_add(1);
            true
        } else {
            false
        }
    }

    #[must_use]
    pub fn model_modal(&self) -> Option<&ModelModal> {
        self.model_modal.as_ref()
    }

    #[must_use]
    pub fn question_modal(&self) -> Option<&QuestionModal> {
        self.question_modal.as_ref()
    }

    pub fn update_turn(&mut self, turn: u32) {
        self.active_turn = turn;
    }

    pub fn add_output_tokens(&mut self, tokens: u64) {
        self.token_count = self.token_count.saturating_add(tokens);
    }

    #[must_use]
    pub fn elapsed_seconds(&self) -> Option<u64> {
        self.run_started_at
            .map(|started_at| started_at.elapsed().as_secs())
    }

    #[must_use]
    pub fn is_running(&self) -> bool {
        self.run_started_at.is_some()
    }

    pub fn record_activity(&mut self, activity: TranscriptActivity) {
        if let Some(id) = activity.id.as_deref()
            && let Some(existing) = self
                .transcript
                .iter_mut()
                .rev()
                .find_map(|entry| match entry {
                    TranscriptEntry::Activity(existing) if existing.id.as_deref() == Some(id) => {
                        Some(existing)
                    }
                    _ => None,
                })
        {
            *existing = activity;
            return;
        }
        self.transcript.push(TranscriptEntry::Activity(activity));
    }

    fn persist_draft(&self) -> io::Result<()> {
        if is_sensitive_draft(&self.composer.draft.text) {
            self.draft_store.delete(&self.draft_key)
        } else {
            self.draft_store.save(&self.draft_key, &self.composer.draft)
        }
    }
}

fn is_sensitive_draft(text: &str) -> bool {
    ["/model key ", "/model api-key "]
        .iter()
        .any(|prefix| text.starts_with(prefix))
}

#[derive(Debug)]
pub enum AppError {
    Clipboard(ClipboardError),
    Io(io::Error),
}

impl std::fmt::Display for AppError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Clipboard(error) => write!(formatter, "{error}"),
            Self::Io(error) => write!(formatter, "{error}"),
        }
    }
}

impl std::error::Error for AppError {}

impl From<ClipboardError> for AppError {
    fn from(error: ClipboardError) -> Self {
        Self::Clipboard(error)
    }
}

impl From<io::Error> for AppError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clipboard::ClipboardImage;
    use tempfile::tempdir;

    struct FakeClipboard(ClipboardContent);

    impl ClipboardService for FakeClipboard {
        fn read(&self) -> Result<ClipboardContent, ClipboardError> {
            Ok(self.0.clone())
        }
    }

    #[test]
    fn explicit_clipboard_text_paste_updates_and_persists_draft() {
        let repository = tempdir().expect("temporary repository");
        let clipboard = Arc::new(FakeClipboard(ClipboardContent::Text(
            "compiler error\nline two".to_owned(),
        )));
        let mut app = AppState::new(
            repository.path().to_path_buf(),
            "session_1",
            "fix this: ",
            clipboard,
        )
        .expect("create app");
        app.paste_from_clipboard().expect("paste clipboard");
        app.persist_draft().expect("save draft");

        let recovered = DraftStore::for_repo(repository.path())
            .load("session_1")
            .expect("load draft")
            .expect("draft exists");
        assert_eq!(recovered.text, "fix this: compiler error\nline two");
    }

    #[test]
    fn ctrl_v_pastes_clipboard_content() {
        let repository = tempdir().expect("temporary repository");
        let clipboard = Arc::new(FakeClipboard(ClipboardContent::Text("pasted".to_owned())));
        let mut app = AppState::new(
            repository.path().to_path_buf(),
            "session_ctrl_v",
            "before ",
            clipboard,
        )
        .expect("create app");
        let action = app
            .handle_event(Event::Key(crossterm::event::KeyEvent::new(
                KeyCode::Char('v'),
                KeyModifiers::CONTROL,
            )))
            .expect("handle Ctrl+V");
        assert_eq!(action, AppAction::Redraw);
        assert_eq!(app.composer.draft.text, "before pasted");
    }

    #[test]
    fn screenshot_paste_creates_visible_attachment_state() {
        let repository = tempdir().expect("temporary repository");
        let clipboard = Arc::new(FakeClipboard(ClipboardContent::Image(ClipboardImage {
            width: 2,
            height: 1,
            rgba: vec![0; 8],
            source_format: Some("image/rgba8".to_owned()),
        })));
        let mut app = AppState::new(repository.path().to_path_buf(), "session_2", "", clipboard)
            .expect("create app");
        app.paste_from_clipboard().expect("paste screenshot");
        assert_eq!(app.composer.draft.attachments.len(), 1);
        assert!(app.status.contains("2×1"));
    }

    #[test]
    fn submit_clears_durable_draft_after_capturing_prompt() {
        let repository = tempdir().expect("temporary repository");
        let clipboard = Arc::new(FakeClipboard(ClipboardContent::Empty));
        let mut app = AppState::new(
            repository.path().to_path_buf(),
            "session_3",
            "fix tests",
            clipboard,
        )
        .expect("create app");
        app.persist_draft().expect("save draft");
        let action = app
            .handle_event(Event::Key(crossterm::event::KeyEvent::new(
                KeyCode::Enter,
                KeyModifiers::NONE,
            )))
            .expect("submit draft");
        assert!(matches!(action, AppAction::Submit(_)));
        assert!(
            DraftStore::for_repo(repository.path())
                .load("session_3")
                .expect("load draft")
                .is_none()
        );
    }

    #[test]
    fn slash_menu_selection_controls_tab_completion() {
        let repository = tempdir().expect("temporary repository");
        let mut app = AppState::new(
            repository.path().to_path_buf(),
            "commands",
            "/",
            Arc::new(FakeClipboard(ClipboardContent::Empty)),
        )
        .expect("create app");
        assert_eq!(
            app.handle_event(Event::Key(crossterm::event::KeyEvent::new(
                KeyCode::Down,
                KeyModifiers::NONE,
            )))
            .expect("select command"),
            AppAction::Redraw
        );
        assert_eq!(app.command_selection, 1);
        assert_eq!(
            app.handle_event(Event::Key(crossterm::event::KeyEvent::new(
                KeyCode::Tab,
                KeyModifiers::NONE,
            )))
            .expect("complete selected command"),
            AppAction::Redraw
        );
        assert_eq!(app.composer.draft.text, "/compact ");
    }

    #[test]
    fn typed_slash_commands_keep_their_name_and_a_bare_slash_stays_in_the_picker() {
        let repository = tempdir().expect("temporary repository");
        let mut app = AppState::new(
            repository.path().to_path_buf(),
            "typed-command",
            "/",
            Arc::new(FakeClipboard(ClipboardContent::Empty)),
        )
        .expect("create app");

        assert_eq!(
            app.handle_event(Event::Key(crossterm::event::KeyEvent::new(
                KeyCode::Enter,
                KeyModifiers::NONE,
            )))
            .expect("submit bare slash"),
            AppAction::Redraw
        );
        assert_eq!(app.composer.draft.text, "/");
        assert!(app.transcript.is_empty());

        for character in ['n', 'e', 'w'] {
            app.handle_event(Event::Key(crossterm::event::KeyEvent::new(
                KeyCode::Char(character),
                KeyModifiers::NONE,
            )))
            .expect("type command");
        }
        assert_eq!(app.composer.draft.text, "/new");
        assert_eq!(
            app.handle_event(Event::Key(crossterm::event::KeyEvent::new(
                KeyCode::Enter,
                KeyModifiers::NONE,
            )))
            .expect("submit command"),
            AppAction::Command(SlashCommand::New)
        );
    }

    #[test]
    fn model_form_requires_explicit_apply_and_updates_effort_and_session_key() {
        let repository = tempdir().expect("temporary repository");
        let mut app = AppState::new(
            repository.path().to_path_buf(),
            "model-picker",
            "/model",
            Arc::new(FakeClipboard(ClipboardContent::Empty)),
        )
        .expect("create app");

        assert_eq!(
            app.handle_event(Event::Key(crossterm::event::KeyEvent::new(
                KeyCode::Enter,
                KeyModifiers::NONE,
            )))
            .expect("open model picker"),
            AppAction::Redraw
        );
        assert_eq!(
            app.model_modal().expect("model picker").focus(),
            ModelModalFocus::Model
        );

        assert_eq!(
            app.handle_event(Event::Key(crossterm::event::KeyEvent::new(
                KeyCode::Char('x'),
                KeyModifiers::NONE,
            )))
            .expect("ignore key input outside the key field"),
            AppAction::Redraw
        );
        assert_eq!(
            app.model_modal().expect("model picker").focus(),
            ModelModalFocus::Model
        );

        app.handle_event(Event::Key(crossterm::event::KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE,
        )))
        .expect("advance to effort");
        assert_eq!(
            app.model_modal().expect("model picker").focus(),
            ModelModalFocus::Effort
        );
        app.handle_event(Event::Key(crossterm::event::KeyEvent::new(
            KeyCode::Up,
            KeyModifiers::NONE,
        )))
        .expect("select medium effort");
        app.handle_event(Event::Key(crossterm::event::KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE,
        )))
        .expect("advance to api key");
        assert_eq!(
            app.model_modal().expect("model picker").focus(),
            ModelModalFocus::ApiKey
        );
        app.handle_event(Event::Paste("replacement-key".to_owned()))
            .expect("paste replacement api key");
        app.handle_event(Event::Key(crossterm::event::KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE,
        )))
        .expect("advance to apply");
        assert_eq!(
            app.model_modal().expect("model picker").focus(),
            ModelModalFocus::Apply
        );

        let action = app
            .handle_event(Event::Key(crossterm::event::KeyEvent::new(
                KeyCode::Enter,
                KeyModifiers::NONE,
            )))
            .expect("submit model configuration");
        let AppAction::ConfigureModel(configuration) = action else {
            panic!("expected a model configuration action");
        };
        assert_eq!(configuration.provider, "minimax");
        assert_eq!(configuration.model, "MiniMax-M3");
        assert_eq!(configuration.effort, Effort::Medium);
        assert_eq!(configuration.api_key.as_deref(), Some("replacement-key"));
        assert!(!format!("{configuration:?}").contains("replacement-key"));
        assert!(app.transcript.is_empty());
    }

    #[test]
    fn active_runs_advance_the_spinner_without_touching_idle_state() {
        let repository = tempdir().expect("temporary repository");
        let mut app = AppState::new(
            repository.path().to_path_buf(),
            "spinner",
            "",
            Arc::new(FakeClipboard(ClipboardContent::Empty)),
        )
        .expect("create app");
        assert!(!app.tick());
        app.begin_run();
        assert!(app.tick());
        assert_eq!(app.spinner_frame, 1);
        app.finish_run();
        assert!(!app.tick());
    }

    #[test]
    fn model_key_command_never_enters_the_transcript() {
        let repository = tempdir().expect("temporary repository");
        let mut app = AppState::new(
            repository.path().to_path_buf(),
            "model-key",
            "/model key secret-value",
            Arc::new(FakeClipboard(ClipboardContent::Empty)),
        )
        .expect("create app");
        let action = app
            .handle_event(Event::Key(crossterm::event::KeyEvent::new(
                KeyCode::Enter,
                KeyModifiers::NONE,
            )))
            .expect("submit key command");
        assert!(matches!(action, AppAction::Command(_)));
        assert!(app.transcript.is_empty());
    }

    #[test]
    fn model_key_text_is_never_autosaved() {
        let repository = tempdir().expect("temporary repository");
        let mut app = AppState::new(
            repository.path().to_path_buf(),
            "secret-draft",
            "/model key ",
            Arc::new(FakeClipboard(ClipboardContent::Empty)),
        )
        .expect("create app");
        app.handle_event(Event::Key(crossterm::event::KeyEvent::new(
            KeyCode::Char('x'),
            KeyModifiers::NONE,
        )))
        .expect("type key character");
        assert!(
            DraftStore::for_repo(repository.path())
                .load("secret-draft")
                .expect("load draft")
                .is_none()
        );
    }

    #[test]
    fn question_modal_tabs_answers_and_requires_confirmation_before_submission() {
        let repository = tempdir().expect("temporary repository");
        let mut app = AppState::new(
            repository.path().to_path_buf(),
            "question",
            "draft text",
            Arc::new(FakeClipboard(ClipboardContent::Empty)),
        )
        .expect("create app");
        app.open_question(vec![
            QuestionPrompt {
                header: "Project".to_owned(),
                question: "Which project should I use?".to_owned(),
                options: vec![
                    QuestionOption {
                        label: "Projects/site-a".to_owned(),
                        description: "Use the existing site".to_owned(),
                    },
                    QuestionOption {
                        label: "Create a new project".to_owned(),
                        description: "Start fresh".to_owned(),
                    },
                ],
                multi_select: false,
            },
            QuestionPrompt {
                header: "Audience".to_owned(),
                question: "Who is this for?".to_owned(),
                options: vec![
                    QuestionOption {
                        label: "Customers".to_owned(),
                        description: "Public visitors".to_owned(),
                    },
                    QuestionOption {
                        label: "Team".to_owned(),
                        description: "Internal users".to_owned(),
                    },
                ],
                multi_select: false,
            },
        ]);
        assert_eq!(
            app.handle_event(Event::Key(crossterm::event::KeyEvent::new(
                KeyCode::Enter,
                KeyModifiers::NONE,
            )))
            .expect("answer first question"),
            AppAction::Redraw
        );
        assert_eq!(
            app.question_modal()
                .expect("question modal")
                .active_question(),
            1
        );
        assert!(app.transcript.is_empty());
        assert_eq!(
            app.handle_event(Event::Key(crossterm::event::KeyEvent::new(
                KeyCode::Enter,
                KeyModifiers::NONE,
            )))
            .expect("answer second question"),
            AppAction::Redraw
        );
        assert!(app.question_modal().expect("review answers").is_reviewing());
        assert!(app.transcript.is_empty());
        let action = app
            .handle_event(Event::Key(crossterm::event::KeyEvent::new(
                KeyCode::Enter,
                KeyModifiers::NONE,
            )))
            .expect("confirm answers");
        assert_eq!(
            action,
            AppAction::AnswerQuestion("Project: Projects/site-a\nAudience: Customers".to_owned())
        );
        assert!(app.question_modal().is_none());
        assert!(matches!(
            app.transcript.last(),
            Some(TranscriptEntry::User(draft))
                if draft.text == "Project: Projects/site-a\nAudience: Customers"
        ));
        assert_eq!(app.composer.draft.text, "draft text");
    }
}
