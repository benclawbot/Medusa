use super::*;
use crate::{clipboard::ClipboardImage, commands::Effort};
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
    assert!(matches!(
        app.transcript.as_slice(),
        [TranscriptEntry::Assistant(text)]
            if text.contains("Which project should I use?")
                && text.contains("Who is this for?")
    ));
    assert_eq!(
        app.handle_event(Event::Key(crossterm::event::KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE,
        )))
        .expect("answer second question"),
        AppAction::Redraw
    );
    assert!(app.question_modal().expect("review answers").is_reviewing());
    assert_eq!(app.transcript.len(), 1);
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

#[test]
fn clarification_question_and_confirmed_answer_stay_in_transcript() {
    let repository = tempdir().expect("temporary repository");
    let mut app = AppState::new(
        repository.path().to_path_buf(),
        "question-transcript",
        "",
        Arc::new(FakeClipboard(ClipboardContent::Empty)),
    )
    .expect("create app");
    app.open_question(vec![QuestionPrompt {
        header: "Audience".to_owned(),
        question: "Who is this for?".to_owned(),
        options: vec![QuestionOption {
            label: "Customers".to_owned(),
            description: "Public visitors".to_owned(),
        }],
        multi_select: false,
    }]);
    assert!(matches!(
        app.transcript.first(),
        Some(TranscriptEntry::Assistant(text))
            if text.contains("Who is this for?") && text.contains("Customers")
    ));
    assert_eq!(
        app.handle_event(Event::Key(crossterm::event::KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE,
        )))
        .expect("select answer"),
        AppAction::Redraw
    );
    let action = app
        .handle_event(Event::Key(crossterm::event::KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE,
        )))
        .expect("confirm answer");
    assert_eq!(
        action,
        AppAction::AnswerQuestion("Audience: Customers".to_owned())
    );
    assert!(matches!(
        app.transcript.last(),
        Some(TranscriptEntry::User(draft)) if draft.text == "Audience: Customers"
    ));
}

#[test]
fn rejected_submission_restores_the_visible_user_draft() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let mut app = AppState::new(
        directory.path().to_path_buf(),
        "restore-rejected",
        "",
        std::sync::Arc::new(crate::clipboard::UnsupportedClipboard),
    )
    .expect("app");
    let draft = PromptDraft {
        text: "additional detail".to_owned(),
        ..PromptDraft::default()
    };
    app.transcript.push(TranscriptEntry::User(draft.clone()));
    app.restore_rejected_submission(draft.clone())
        .expect("restore submission");
    assert_eq!(app.composer.draft, draft);
    assert!(app.transcript.is_empty());
}
