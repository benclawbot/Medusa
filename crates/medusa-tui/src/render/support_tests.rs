use super::*;

#[test]
fn user_prompt_text_is_readable_on_a_dark_terminal() {
    let directory = tempfile::tempdir().expect("tempdir");
    let mut app = AppState::new(
        directory.path().to_path_buf(),
        "user-prompt-contrast",
        "",
        Arc::new(UnsupportedClipboard),
    )
    .expect("app");
    app.transcript.push(TranscriptEntry::User(PromptDraft {
        text: "make it into html".to_owned(),
        ..PromptDraft::default()
    }));

    let prompt = transcript_lines(&app, 80)
        .into_iter()
        .find(|line| line.text == "make it into html")
        .expect("rendered user prompt");

    assert_ne!(prompt.foreground, Color::Black);
}

#[test]
fn conversation_urls_are_emitted_as_terminal_hyperlinks() {
    let rendered = terminal_hyperlinks("See https://example.com/docs.");
    assert!(rendered.contains("\x1b]8;;https://example.com/docs\x1b\\"));
    assert!(rendered.ends_with("\x1b]8;;\x1b\\."));
}
