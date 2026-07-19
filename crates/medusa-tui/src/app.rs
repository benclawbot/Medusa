use std::{
    io,
    path::{Path, PathBuf},
    sync::Arc,
    time::Instant,
};

use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers, MouseEventKind};

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

mod modal_events;
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
    repository: PathBuf,
    pub composer: ComposerState,
    pub transcript: Vec<TranscriptEntry>,
    pub plan: Option<TranscriptPlan>,
    pub status: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub timed_output_tokens: u64,
    pub cache_read_input_tokens: u64,
    pub cache_creation_input_tokens: u64,
    pub model_elapsed_millis: u64,
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
    session_started_at: Instant,
    session_elapsed_seconds: u64,
    run_started_at: Option<Instant>,
    draft_store: DraftStore,
    draft_key: String,
    clipboard: Arc<dyn ClipboardService>,
}

impl AppState {
    #[must_use]
    pub(crate) fn repository(&self) -> &Path {
        &self.repository
    }

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
            repository,
            composer,
            transcript: Vec::new(),
            plan: None,
            status: "ready".to_owned(),
            input_tokens: 0,
            output_tokens: 0,
            timed_output_tokens: 0,
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
            model_elapsed_millis: 0,
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
            session_started_at: Instant::now(),
            session_elapsed_seconds: 0,
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
            match key.code {
                KeyCode::PageUp => {
                    self.scrollback_scroll_up(10, usize::MAX);
                    return Ok(AppAction::Redraw);
                }
                KeyCode::PageDown => {
                    self.scrollback_scroll_down(10);
                    return Ok(AppAction::Redraw);
                }
                KeyCode::Home if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    self.set_scrollback_offset(usize::MAX);
                    return Ok(AppAction::Redraw);
                }
                KeyCode::End if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    self.set_scrollback_offset(0);
                    return Ok(AppAction::Redraw);
                }
                KeyCode::Esc => return Ok(AppAction::Quit),
                _ => {}
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

        if let Event::Mouse(mouse) = event {
            match mouse.kind {
                MouseEventKind::ScrollUp => {
                    self.scrollback_scroll_up(3, usize::MAX);
                    return Ok(AppAction::Redraw);
                }
                MouseEventKind::ScrollDown => {
                    self.scrollback_scroll_down(3);
                    return Ok(AppAction::Redraw);
                }
                _ => return Ok(AppAction::None),
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
                            let agent_task = match &command {
                                SlashCommand::Plan { task: Some(task) }
                                | SlashCommand::Skill {
                                    task: Some(task), ..
                                } => Some(task),
                                _ => None,
                            };
                            if let Some(task) = agent_task {
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
                self.set_scrollback_offset(0);
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
        self.input_tokens = 0;
        self.output_tokens = 0;
        self.timed_output_tokens = 0;
        self.cache_read_input_tokens = 0;
        self.cache_creation_input_tokens = 0;
        self.model_elapsed_millis = 0;
        self.active_turn = 0;
        self.session_started_at = Instant::now();
        self.session_elapsed_seconds = 0;
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

    pub fn record_assistant_text(&mut self, text: String) {
        let text = text.trim_end().to_owned();
        if !text.trim().is_empty() {
            self.transcript.push(TranscriptEntry::Assistant(text));
        }
    }

    pub fn restore_rejected_submission(&mut self, draft: PromptDraft) -> io::Result<()> {
        if matches!(self.transcript.last(), Some(TranscriptEntry::User(existing)) if existing == &draft)
        {
            self.transcript.pop();
        }
        let cursor = draft.text.len();
        self.composer = ComposerState { draft, cursor };
        self.persist_draft()
    }

    pub fn open_question(&mut self, questions: Vec<QuestionPrompt>) {
        let question_text = questions
            .iter()
            .map(|prompt| {
                let mut lines = vec![format!("{}: {}", prompt.header, prompt.question)];
                if !prompt.options.is_empty() {
                    lines.push(format!(
                        "Options: {}",
                        prompt
                            .options
                            .iter()
                            .map(|option| option.label.as_str())
                            .collect::<Vec<_>>()
                            .join(" · ")
                    ));
                }
                lines.join("\n")
            })
            .collect::<Vec<_>>()
            .join("\n\n");
        self.record_assistant_text(question_text);
        self.question_modal = Some(QuestionModal::new(questions));
        self.status = "waiting for your answer".to_owned();
        self.finish_run();
    }

    fn select_command(&mut self, direction: isize) -> Result<AppAction, AppError> {
        let suggestions = command_suggestions(&self.composer.draft.text, &self.repository);
        if suggestions.is_empty() {
            return Ok(AppAction::None);
        }
        let length = suggestions.len() as isize;
        self.command_selection =
            (self.command_selection as isize + direction).rem_euclid(length) as usize;
        Ok(AppAction::Redraw)
    }

    fn complete_command(&mut self) -> Result<AppAction, AppError> {
        let suggestions = command_suggestions(&self.composer.draft.text, &self.repository);
        let Some(selected) = suggestions.get(self.command_selection) else {
            return Ok(AppAction::None);
        };
        let completed = if self.command_selection == 0 {
            complete_first_command(&self.composer.draft.text, &self.repository)
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
        self.active_turn = 0;
        self.spinner_frame = 0;
        self.run_started_at = Some(Instant::now());
    }

    pub fn finish_run(&mut self) {
        self.run_started_at = None;
    }

    pub fn tick(&mut self) -> bool {
        let elapsed = self.session_started_at.elapsed().as_secs();
        let session_changed = elapsed != self.session_elapsed_seconds;
        self.session_elapsed_seconds = elapsed;
        let spinner_changed = self.is_running();
        if spinner_changed {
            self.spinner_frame = self.spinner_frame.wrapping_add(1);
        }
        session_changed || spinner_changed
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

    pub fn record_usage(
        &mut self,
        input_tokens: u64,
        output_tokens: u64,
        cache_read_input_tokens: u64,
        cache_creation_input_tokens: u64,
        model_elapsed_millis: u64,
    ) {
        self.input_tokens = self.input_tokens.saturating_add(input_tokens);
        self.output_tokens = self.output_tokens.saturating_add(output_tokens);
        if model_elapsed_millis > 0 {
            self.timed_output_tokens = self.timed_output_tokens.saturating_add(output_tokens);
        }
        self.cache_read_input_tokens = self
            .cache_read_input_tokens
            .saturating_add(cache_read_input_tokens);
        self.cache_creation_input_tokens = self
            .cache_creation_input_tokens
            .saturating_add(cache_creation_input_tokens);
        self.model_elapsed_millis = self
            .model_elapsed_millis
            .saturating_add(model_elapsed_millis);
    }

    #[must_use]
    pub fn total_input_tokens(&self) -> u64 {
        self.input_tokens
            .saturating_add(self.cache_read_input_tokens)
            .saturating_add(self.cache_creation_input_tokens)
    }

    #[must_use]
    pub fn cache_read_percentage(&self) -> f64 {
        let total = self.total_input_tokens();
        if total == 0 {
            return 0.0;
        }
        self.cache_read_input_tokens as f64 * 100.0 / total as f64
    }

    #[must_use]
    pub fn output_tokens_per_second(&self) -> Option<f64> {
        (self.model_elapsed_millis > 0)
            .then(|| self.timed_output_tokens as f64 * 1_000.0 / self.model_elapsed_millis as f64)
    }

    #[must_use]
    pub fn session_elapsed_seconds(&self) -> u64 {
        self.session_elapsed_seconds
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
