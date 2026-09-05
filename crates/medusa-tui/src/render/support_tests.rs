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
    assert_eq!(prompt.marker, Some(("› ".to_owned(), Color::White)));
    assert_eq!(prompt.background, Some(Color::DarkGrey));
    assert!(prompt.fill_background);
}

#[test]
fn conversation_role_labels_are_omitted() {
    let directory = tempfile::tempdir().expect("tempdir");
    let mut app = AppState::new(
        directory.path().to_path_buf(),
        "role-labels",
        "",
        Arc::new(UnsupportedClipboard),
    )
    .expect("app");
    app.transcript.push(TranscriptEntry::User(PromptDraft {
        text: "question".to_owned(),
        ..PromptDraft::default()
    }));
    app.transcript
        .push(TranscriptEntry::Assistant("answer".to_owned()));

    let visible = transcript_lines(&app, 80)
        .into_iter()
        .map(|line| line.visible_text(80))
        .collect::<Vec<_>>()
        .join("\n");

    assert!(!visible.contains("You"));
    assert!(!visible.contains("Medusa"));
    assert!(visible.contains("› question"));
    assert!(visible.contains("answer"));
}

#[test]
fn worked_for_separator_matches_codex_style_and_width() {
    let line = worked_for_line(64, 80);

    assert!(line.text.starts_with("─ Worked for 1m 04s "));
    assert_eq!(line.text.chars().count(), 80);
    assert!(
        line.text["─ Worked for 1m 04s ".len()..]
            .chars()
            .all(|character| character == '─')
    );
    assert_eq!(line.foreground, Color::DarkGrey);
}

#[test]
fn worked_for_separator_formats_hours() {
    let line = worked_for_line(3_661, 80);
    assert!(line.text.starts_with("─ Worked for 1h 01m 01s "));
}

#[test]
fn finishing_a_run_adds_only_one_worked_for_separator() {
    let directory = tempfile::tempdir().expect("tempdir");
    let mut app = AppState::new(
        directory.path().to_path_buf(),
        "turn-finished",
        "",
        Arc::new(UnsupportedClipboard),
    )
    .expect("app");

    app.begin_run();
    app.finish_run();
    app.finish_run();

    let separators = transcript_lines(&app, 80)
        .into_iter()
        .filter(|line| line.text.starts_with("─ Worked for "))
        .collect::<Vec<_>>();
    assert_eq!(separators.len(), 1);
    assert_eq!(separators[0].text.chars().count(), 80);
}

#[test]
fn conversation_urls_are_emitted_as_terminal_hyperlinks() {
    let rendered = terminal_hyperlinks("See https://example.com/docs.");
    assert!(rendered.contains("\x1b]8;;https://example.com/docs\x1b\\"));
    assert!(rendered.ends_with("\x1b]8;;\x1b\\."));
}

#[test]
fn fixed_frame_rows_never_wrap_into_extra_terminal_lines() {
    let status = "session 23s · total 0 · input 0 · output 0 · cache-read 0 · cache-write 0 · cost — · — · — tok/s · confirmation [Full Access]";
    let row = fit_to_row(status, 80);

    assert_eq!(row.chars().count(), 80);
    assert!(!row.contains('\n'));
    assert_eq!(fit_to_row("status\nnext row", 80), "status");
}

#[test]
fn verbosity_filters_tool_activity_rows() {
    use crate::app::TranscriptActivityKind;

    let directory = tempfile::tempdir().expect("tempdir");
    let mut app = AppState::new(
        directory.path().to_path_buf(),
        "verbosity-filter",
        "",
        Arc::new(UnsupportedClipboard),
    )
    .expect("app");
    for (index, title) in ["first tool call", "second tool call"]
        .into_iter()
        .enumerate()
    {
        app.transcript
            .push(TranscriptEntry::Activity(TranscriptActivity {
                id: Some(format!("tool-{index}")),
                kind: TranscriptActivityKind::Progress,
                title: title.to_owned(),
                details: vec!["detail".to_owned()],
            }));
    }
    app.transcript
        .push(TranscriptEntry::Activity(TranscriptActivity {
            id: None,
            kind: TranscriptActivityKind::Error,
            title: "boom".to_owned(),
            details: Vec::new(),
        }));

    let titles = |app: &AppState| {
        transcript_lines(app, 80)
            .into_iter()
            .map(|line| line.text)
            .collect::<Vec<_>>()
    };

    app.verbosity = Verbosity::All;
    let all = titles(&app).join("\n");
    assert!(all.contains("first tool call") && all.contains("second tool call"));

    app.verbosity = Verbosity::Off;
    let off = titles(&app).join("\n");
    assert!(!off.contains("tool call"));
    assert!(off.contains("boom"));

    app.verbosity = Verbosity::New;
    let new = titles(&app).join("\n");
    assert!(!new.contains("first tool call"));
    assert!(new.contains("second tool call"));

    app.verbosity = Verbosity::Verbose;
    let verbose = titles(&app).join("\n");
    assert!(verbose.contains("detail"));
}
