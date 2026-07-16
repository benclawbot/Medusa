use std::{io, path::PathBuf, sync::Arc, time::Instant};

use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};

use crate::{
    clipboard::{
        ClipboardContent, ClipboardError, ClipboardService, FileAttachment, PromptAttachment,
        PromptDraft,
    },
    commands::{
        ModelCommand, SlashCommand, command_suggestions, complete_first_command,
        parse_slash_command,
    },
    draft_store::DraftStore,
    input::{ComposerAction, ComposerState},
};

mod models;
#[cfg(test)]
mod tests;

pub use models::*;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Scrollback {
    pub offset: usize,
}

impl Scrollback {
    /// Increase the offset by `step`, capped by `max_offset`.
    pub fn scroll_up(&mut self, step: usize, max_offset: usize) {
        self.offset = self.offset.saturating_add(step).min(max_offset);
    }

    /// Decrease the offset by `step`, clamped at 0.
    pub fn scroll_down(&mut self, step: usize) {
        self.offset = self.offset.saturating_sub(step);
    }
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
    pub scrollback: Scrollback,
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
    #[must_use]
    pub fn scrollback_offset(&self) -> usize {
        self.scrollback.offset
    }

    pub fn set_scrollback_offset(&mut self, offset: usize) {
        self.scrollback.offset = offset;
    }

    pub fn scrollback_scroll_up(&mut self, step: usize, max_offset: usize) {
        self.scrollback.scroll_up(step, max_offset);
    }

    pub fn scrollback_scroll_down(&mut self, step: usize) {
        self.scrollback.scroll_down(step);
    }

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
            scrollback: Scrollback::default(),
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

    pub fn dismiss_welcome_for_event(&mut self, event: &Event) -> bool {
        let is_user_input = matches!(event, Event::Key(key) if key.kind != KeyEventKind::Release)
            || matches!(event, Event::Paste(_) | Event::Mouse(_));
        if self.welcome_visible && is_user_input {
            self.welcome_visible = false;
            return true;
        }
        false
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
                        .is_some_and(|modal| modal.focus() == ModelModalFocus::Apply);
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
                        && modal.focus() == ModelModalFocus::ApiKey
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
                        && modal.focus() == ModelModalFocus::ApiKey
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
