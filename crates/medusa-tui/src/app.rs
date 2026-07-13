use std::{io, path::PathBuf, sync::Arc, time::Instant};

use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};

use crate::{
    clipboard::{
        ClipboardContent, ClipboardError, ClipboardService, FileAttachment, PromptAttachment,
        PromptDraft,
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
    Interrupt,
    Quit,
}

pub struct AppState {
    pub composer: ComposerState,
    pub transcript: Vec<TranscriptEntry>,
    pub plan: Option<TranscriptPlan>,
    pub status: String,
    pub token_count: u64,
    pub active_turn: u32,
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
            run_started_at: None,
            draft_store,
            draft_key,
            clipboard,
        })
    }

    pub fn handle_event(&mut self, event: Event) -> Result<AppAction, AppError> {
        if let Event::Key(key) = &event
            && key.kind == KeyEventKind::Press
        {
            if key.code == KeyCode::Esc {
                return Ok(AppAction::Quit);
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
                self.persist_draft()?;
                Ok(AppAction::Redraw)
            }
            ComposerAction::Interrupt => Ok(AppAction::Interrupt),
            ComposerAction::Submit => {
                let submitted = self.composer.draft.clone();
                self.transcript
                    .push(TranscriptEntry::User(submitted.clone()));
                self.plan = None;
                self.composer = ComposerState::new("");
                self.draft_store.delete(&self.draft_key)?;
                self.status = "prompt submitted".to_owned();
                Ok(AppAction::Submit(submitted))
            }
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
        self.run_started_at = Some(Instant::now());
    }

    pub fn finish_run(&mut self) {
        self.run_started_at = None;
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
        self.draft_store.save(&self.draft_key, &self.composer.draft)
    }
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
}
