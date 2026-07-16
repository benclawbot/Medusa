use std::sync::Arc;

use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use medusa_tui::{
    app::{AppAction, AppState, ModelModalFocus, QuestionOption, QuestionPrompt, Scrollback},
    clipboard::{ClipboardContent, ClipboardError, ClipboardService},
    commands::Effort,
};
use tempfile::tempdir;

struct EmptyClipboard;

impl ClipboardService for EmptyClipboard {
    fn read(&self) -> Result<ClipboardContent, ClipboardError> {
        Ok(ClipboardContent::Empty)
    }
}

fn key(code: KeyCode) -> Event {
    Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
}

#[test]
fn public_model_modal_flow_covers_provider_effort_and_key_boundaries() {
    let repository = tempdir().expect("temporary repository");
    let mut app = AppState::new(
        repository.path().to_path_buf(),
        "modal-coverage",
        "/model",
        Arc::new(EmptyClipboard),
    )
    .expect("create app");

    assert_eq!(app.handle_event(key(KeyCode::Enter)).expect("open modal"), AppAction::Redraw);
    let modal = app.model_modal().expect("model modal");
    assert_eq!(modal.provider(), "minimax");
    assert_eq!(modal.selected_model(), "MiniMax-M3");
    assert_eq!(modal.api_key_mask(), "not configured");

    app.handle_event(key(KeyCode::BackTab)).expect("focus provider");
    assert_eq!(app.model_modal().expect("modal").focus(), ModelModalFocus::Provider);
    app.handle_event(key(KeyCode::Right)).expect("select provider");
    assert_eq!(app.model_modal().expect("modal").provider(), "anthropic");

    app.handle_event(key(KeyCode::Tab)).expect("focus model");
    app.handle_event(key(KeyCode::Down)).expect("select model");
    assert_eq!(
        app.model_modal().expect("modal").selected_model(),
        "claude-sonnet-4-6"
    );

    app.handle_event(key(KeyCode::Tab)).expect("focus effort");
    app.handle_event(key(KeyCode::Down)).expect("select auto effort");
    assert_eq!(app.model_modal().expect("modal").effort(), Effort::Auto);

    app.handle_event(key(KeyCode::Tab)).expect("focus key");
    app.handle_event(Event::Paste(" key with spaces ".to_owned()))
        .expect("paste key");
    assert_eq!(app.model_modal().expect("modal").api_key_mask(), "*************");
    app.handle_event(key(KeyCode::Backspace)).expect("delete key character");
    app.handle_event(key(KeyCode::Enter)).expect("focus apply");

    let action = app.handle_event(key(KeyCode::Enter)).expect("apply model");
    let AppAction::ConfigureModel(configuration) = action else {
        panic!("expected model configuration");
    };
    assert_eq!(configuration.provider, "anthropic");
    assert_eq!(configuration.model, "claude-sonnet-4-6");
    assert_eq!(configuration.effort, Effort::Auto);
    assert_eq!(configuration.api_key.as_deref(), Some("keywithspace"));
}

#[test]
fn public_question_modal_flow_covers_multiselect_custom_and_review_paths() {
    let repository = tempdir().expect("temporary repository");
    let mut app = AppState::new(
        repository.path().to_path_buf(),
        "question-coverage",
        "",
        Arc::new(EmptyClipboard),
    )
    .expect("create app");

    app.open_question(vec![QuestionPrompt {
        header: "Scope".to_owned(),
        question: "Choose targets".to_owned(),
        options: vec![
            QuestionOption {
                label: "A".to_owned(),
                description: String::new(),
            },
            QuestionOption {
                label: "B".to_owned(),
                description: String::new(),
            },
        ],
        multi_select: true,
    }]);

    app.handle_event(key(KeyCode::Char(' '))).expect("select A");
    app.handle_event(key(KeyCode::Right)).expect("move to B");
    app.handle_event(key(KeyCode::Char(' '))).expect("select B");
    assert_eq!(app.question_modal().expect("modal").answer_for(0).as_deref(), Some("A, B"));

    app.handle_event(key(KeyCode::Enter)).expect("review answers");
    assert!(app.question_modal().expect("modal").is_reviewing());
    app.handle_event(key(KeyCode::Esc)).expect("return to question");
    assert!(!app.question_modal().expect("modal").is_reviewing());

    app.handle_event(Event::Paste("custom".to_owned()))
        .expect("enter custom answer");
    app.handle_event(key(KeyCode::Backspace)).expect("delete character");
    assert_eq!(app.question_modal().expect("modal").active_custom_answer(), "custo");
}

#[test]
fn scrollback_clamps_at_both_boundaries() {
    let mut scrollback = Scrollback::default();
    scrollback.scroll_up(10, 4);
    assert_eq!(scrollback.offset, 4);
    scrollback.scroll_down(2);
    assert_eq!(scrollback.offset, 2);
    scrollback.scroll_down(10);
    assert_eq!(scrollback.offset, 0);
}
