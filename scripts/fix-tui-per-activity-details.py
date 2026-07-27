#!/usr/bin/env python3
from pathlib import Path

app_path = Path("crates/medusa-tui/src/app.rs")
app = app_path.read_text()
old_reset = "        self.activity_details_expanded = false;"
if old_reset not in app:
    raise SystemExit("activity detail reset anchor not found")
app = app.replace(old_reset, "        self.expanded_activity_details.clear();", 1)
app_path.write_text(app)

tests_path = Path("crates/medusa-tui/src/app/tests.rs")
tests = tests_path.read_text()
old_test = '''#[test]
fn ctrl_e_toggles_activity_detail_expansion_and_new_session_resets_it() {
    let repository = tempdir().expect("temporary repository");
    let mut app = AppState::new(
        repository.path().to_path_buf(),
        "details-toggle",
        "",
        Arc::new(FakeClipboard(ClipboardContent::Empty)),
    )
    .expect("create app");

    assert!(!app.activity_details_expanded);
    assert_eq!(
        app.handle_event(Event::Key(KeyEvent::new(
            KeyCode::Char('e'),
            KeyModifiers::CONTROL,
        )))
        .expect("toggle details"),
        AppAction::Redraw
    );
    assert!(app.activity_details_expanded);

    app.clear_for_new_session();
    assert!(!app.activity_details_expanded);
}
'''
new_test = '''#[test]
fn ctrl_e_toggles_activity_detail_expansion_and_new_session_resets_it() {
    let repository = tempdir().expect("temporary repository");
    let mut app = AppState::new(
        repository.path().to_path_buf(),
        "details-toggle",
        "",
        Arc::new(FakeClipboard(ClipboardContent::Empty)),
    )
    .expect("create app");
    app.transcript.push(TranscriptEntry::Activity(TranscriptActivity {
        id: Some("details".to_owned()),
        kind: TranscriptActivityKind::Verification,
        title: "Verification".to_owned(),
        details: vec!["detail".to_owned()],
    }));
    let activity = match &app.transcript[0] {
        TranscriptEntry::Activity(activity) => activity,
        _ => unreachable!(),
    };
    assert!(!app.activity_details_expanded(0, activity));
    assert_eq!(
        app.handle_event(Event::Key(KeyEvent::new(
            KeyCode::Char('e'),
            KeyModifiers::CONTROL,
        )))
        .expect("toggle details"),
        AppAction::Redraw
    );
    let activity = match &app.transcript[0] {
        TranscriptEntry::Activity(activity) => activity,
        _ => unreachable!(),
    };
    assert!(app.activity_details_expanded(0, activity));

    app.clear_for_new_session();
    assert!(app.activity_detail_expansion_snapshot().is_empty());
}
'''
if old_test not in tests:
    raise SystemExit("Ctrl+E compatibility test anchor not found")
tests = tests.replace(old_test, new_test, 1)
tests_path.write_text(tests)
