from pathlib import Path


def replace_once(label: str, path: str, old: str, new: str) -> None:
    print(f"apply: {label}")
    target = Path(path)
    text = target.read_text()
    if new in text:
        print(f"already applied: {label}")
        return
    count = text.count(old)
    if count != 1:
        raise SystemExit(f"{label}: {path}: expected one old snippet, found {count}")
    target.write_text(text.replace(old, new, 1))


def remove_function(label: str, path: str, function_name: str) -> None:
    print(f"remove: {label}")
    target = Path(path)
    text = target.read_text()
    needle = f"fn {function_name}("
    start = text.find(needle)
    if start < 0:
        print(f"already removed: {label}")
        return
    line_start = text.rfind("\n", 0, start) + 1
    brace = text.find("{", start)
    depth = 0
    end = None
    for index in range(brace, len(text)):
        if text[index] == "{":
            depth += 1
        elif text[index] == "}":
            depth -= 1
            if depth == 0:
                end = index + 1
                break
    if end is None:
        raise SystemExit(f"{label}: could not find function end")
    while end < len(text) and text[end] == "\n":
        end += 1
    target.write_text(text[:line_start] + text[end:])


def replace_test_function(label: str, path: str, function_name: str, replacement: str) -> None:
    print(f"replace test: {label}")
    target = Path(path)
    text = target.read_text()
    if replacement in text:
        print(f"already applied: {label}")
        return
    needle = f"fn {function_name}("
    start = text.find(needle)
    if start < 0:
        raise SystemExit(f"{label}: test function not found")
    attribute_start = text.rfind("#[test]", 0, start)
    if attribute_start < 0:
        raise SystemExit(f"{label}: test attribute not found")
    brace = text.find("{", start)
    depth = 0
    end = None
    for index in range(brace, len(text)):
        if text[index] == "{":
            depth += 1
        elif text[index] == "}":
            depth -= 1
            if depth == 0:
                end = index + 1
                break
    if end is None:
        raise SystemExit(f"{label}: test function end not found")
    target.write_text(text[:attribute_start] + replacement + text[end:])


def append_test(label: str, path: str, unique_name: str, test_source: str) -> None:
    print(f"append test: {label}")
    target = Path(path)
    text = target.read_text()
    if unique_name in text:
        print(f"already applied: {label}")
        return
    closing = text.rfind("\n}")
    if closing < 0:
        raise SystemExit(f"{label}: module closing brace not found")
    target.write_text(text[:closing] + "\n\n" + test_source.rstrip() + text[closing:])


replace_once(
    "transcript assistant variant",
    "crates/medusa-tui/src/app/models.rs",
    """pub enum TranscriptEntry {
    User(PromptDraft),
    Activity(TranscriptActivity),
    System(String),
}""",
    """pub enum TranscriptEntry {
    User(PromptDraft),
    Assistant(String),
    Activity(TranscriptActivity),
    System(String),
}""",
)

replace_once(
    "runtime assistant text event",
    "crates/medusa-tui/src/runtime.rs",
    """pub enum RuntimeEvent {
    Started,
    Activity(RuntimeActivity),
    Plan(TranscriptPlan),""",
    """pub enum RuntimeEvent {
    Started,
    AssistantText(String),
    Activity(RuntimeActivity),
    Plan(TranscriptPlan),""",
)

replace_once(
    "forward full assistant text",
    "crates/medusa-tui/src/runtime/support.rs",
    """        // Keep the assistant's milestone, but not the expanded narrative that follows it.
        // Tool arguments and results remain in the durable session for the model.
        AgentUpdate::AssistantText(text) => {
            if let Some(title) = assistant_title(text) {
                let _ = events.send(RuntimeEvent::Activity(RuntimeActivity {
                    id: None,
                    kind: RuntimeActivityKind::Assistant,
                    title,
                    details: Vec::new(),
                }));
            }
        }""",
    """        AgentUpdate::AssistantText(text) => {
            if !text.trim().is_empty() {
                let _ = events.send(RuntimeEvent::AssistantText(text.clone()));
            }
        }""",
)

remove_function(
    "obsolete assistant headline helper",
    "crates/medusa-tui/src/runtime/support.rs",
    "assistant_title",
)

replace_once(
    "obsolete assistant headline assertions",
    "crates/medusa-tui/src/runtime/support.rs",
    """        assert_eq!(
            assistant_title("\n## Milestone reached\nmore"),
            Some("Milestone reached".to_owned())
        );
        assert_eq!(assistant_title("   \n"), None);
""",
    "",
)

replace_once(
    "record assistant text and clarification questions",
    "crates/medusa-tui/src/app.rs",
    """    pub fn open_question(&mut self, questions: Vec<QuestionPrompt>) {
        self.question_modal = Some(QuestionModal::new(questions));
        self.status = "waiting for your answer".to_owned();
        self.finish_run();
    }""",
    """    pub fn record_assistant_text(&mut self, text: String) {
        let text = text.trim_end().to_owned();
        if !text.trim().is_empty() {
            self.transcript.push(TranscriptEntry::Assistant(text));
        }
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
    }""",
)

replace_once(
    "apply assistant text event",
    "crates/medusa-tui/src/session.rs",
    """            RuntimeEvent::Activity(activity) => {
                app.record_activity(TranscriptActivity {""",
    """            RuntimeEvent::AssistantText(text) => {
                app.record_assistant_text(text);
            }
            RuntimeEvent::Activity(activity) => {
                app.record_activity(TranscriptActivity {""",
)

replace_once(
    "reuse width-aware transcript in legacy renderer",
    "crates/medusa-tui/src/render.rs",
    """    let mut lines = Vec::new();
    for entry in &app.transcript {
        match entry {
            TranscriptEntry::User(draft) => {
                lines.push(StyledLine::with_marker(
                    "> ",
                    Color::Cyan,
                    draft.text.replace('\n', " / "),
                    Color::White,
                ));
                lines.extend(draft.attachments.iter().map(|attachment| {
                    StyledLine::new(
                        format!("  └ {}", attachment_label(attachment)),
                        Color::DarkGrey,
                    )
                }));
            }
            TranscriptEntry::Activity(activity) => lines.extend(activity_lines(activity)),
            TranscriptEntry::System(message) => lines.push(system_line(message)),
        }
    }""",
    """    let mut lines = transcript_lines(app, width);""",
)

replace_once(
    "pass terminal width to transcript renderer",
    "crates/medusa-tui/src/render.rs",
    "let mut content = transcript_lines(app);",
    "let mut content = transcript_lines(app, width);",
)

replace_once(
    "render full labelled conversation blocks",
    "crates/medusa-tui/src/render/support.rs",
    """pub(super) fn transcript_lines(app: &AppState) -> Vec<StyledLine> {
    let mut lines = Vec::new();
    for entry in &app.transcript {
        match entry {
            TranscriptEntry::User(draft) => {
                lines.push(StyledLine::with_marker(
                    "> ",
                    Color::Cyan,
                    draft.text.replace('\n', " / "),
                    Color::White,
                ));
                lines.extend(draft.attachments.iter().map(|attachment| {
                    StyledLine::new(
                        format!("  - {}", attachment_label(attachment)),
                        Color::DarkGrey,
                    )
                }));
            }
            TranscriptEntry::Activity(activity) => lines.extend(activity_lines(activity)),
            TranscriptEntry::System(message) => lines.push(system_line(message)),
        }
    }
    lines
}""",
    """pub(super) fn transcript_lines(app: &AppState, width: u16) -> Vec<StyledLine> {
    let mut lines = Vec::new();
    for entry in &app.transcript {
        match entry {
            TranscriptEntry::User(draft) => {
                let text = if draft.text.is_empty() {
                    "(attachment-only prompt)"
                } else {
                    &draft.text
                };
                lines.extend(conversation_block_lines(
                    "You     ",
                    Color::Cyan,
                    text,
                    Color::Grey,
                    width,
                ));
                for attachment in &draft.attachments {
                    lines.extend(conversation_block_lines(
                        "        ",
                        Color::DarkGrey,
                        &format!("[attachment] {}", attachment_label(attachment)),
                        Color::DarkGrey,
                        width,
                    ));
                }
            }
            TranscriptEntry::Assistant(text) => lines.extend(conversation_block_lines(
                "Medusa  ",
                Color::Magenta,
                text,
                Color::White,
                width,
            )),
            TranscriptEntry::Activity(activity) => lines.extend(activity_lines(activity)),
            TranscriptEntry::System(message) => lines.push(system_line(message)),
        }
    }
    lines
}

fn conversation_block_lines(
    first_marker: &str,
    marker_color: Color,
    text: &str,
    foreground: Color,
    width: u16,
) -> Vec<StyledLine> {
    let marker_width = first_marker.chars().count();
    let content_width = usize::from(width).saturating_sub(marker_width).max(1);
    let continuation = " ".repeat(marker_width);
    let mut visual_rows = Vec::new();
    for source_line in text.split('\n') {
        if source_line.is_empty() {
            visual_rows.push(String::new());
            continue;
        }
        let characters = source_line.chars().collect::<Vec<_>>();
        visual_rows.extend(
            characters
                .chunks(content_width)
                .map(|chunk| chunk.iter().collect::<String>()),
        );
    }
    if visual_rows.is_empty() {
        visual_rows.push(String::new());
    }
    visual_rows
        .into_iter()
        .enumerate()
        .map(|(index, row)| {
            StyledLine::with_marker(
                if index == 0 {
                    first_marker.to_owned()
                } else {
                    continuation.clone()
                },
                marker_color,
                row,
                foreground,
            )
        })
        .collect()
}""",
)

replace_test_function(
    "assistant text runtime contract",
    "crates/medusa-tui/src/runtime/tests.rs",
    "internal_plan_transport_is_hidden_and_assistant_narration_is_one_headline",
    """#[test]
fn internal_plan_transport_is_hidden_and_assistant_text_is_forwarded_verbatim() {
    let (sender, receiver) = mpsc::channel();
    let mut state = UpdateState::new();
    forward_update(
        &AgentUpdate::Event(EventPayload::ToolCallRequested {
            tool: "update_plan".to_owned(),
            arguments: json!({"steps": [{"title": "Inspect", "status": "active"}]}),
        }),
        &sender,
        &mut state,
    );
    assert!(matches!(
        receiver.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));
    forward_update(
        &AgentUpdate::AssistantText(
            "Now I have a clear picture. Key findings:\n\n1. First detail\n2. Second detail"
                .to_owned(),
        ),
        &sender,
        &mut state,
    );
    assert_eq!(
        receiver.recv().expect("assistant text"),
        RuntimeEvent::AssistantText(
            "Now I have a clear picture. Key findings:\n\n1. First detail\n2. Second detail"
                .to_owned()
        )
    );
}
""",
)

append_test(
    "spacebar regression",
    "crates/medusa-tui/src/input.rs",
    "fn spacebar_is_inserted_as_regular_text",
    """    #[test]
    fn spacebar_is_inserted_as_regular_text() {
        let mut composer = ComposerState::new("hello");
        composer
            .handle_event(Event::Key(KeyEvent::new(
                KeyCode::Char(' '),
                KeyModifiers::NONE,
            )))
            .expect("spacebar");
        composer
            .handle_event(Event::Key(KeyEvent::new(
                KeyCode::Char('w'),
                KeyModifiers::NONE,
            )))
            .expect("word");
        assert_eq!(composer.draft.text, "hello w");
    }
""",
)

append_test(
    "multiline conversation rendering",
    "crates/medusa-tui/src/lib.rs",
    "fn conversation_preserves_user_and_medusa_multiline_text",
    """    #[test]
    fn conversation_preserves_user_and_medusa_multiline_text() {
        let directory = tempfile::tempdir().expect("tempdir");
        let mut app = AppState::new(
            directory.path().to_path_buf(),
            "conversation",
            "",
            Arc::new(UnsupportedClipboard),
        )
        .expect("app");
        app.transcript.push(TranscriptEntry::User(PromptDraft {
            text: "first user line\nsecond user line".to_owned(),
            ..PromptDraft::default()
        }));
        app.record_assistant_text("first answer line\n\nsecond answer line".to_owned());
        let lines = transcript_lines(&app, 80);
        let text = lines
            .iter()
            .map(|line| line.text.as_str())
            .collect::<Vec<_>>();
        assert!(text.contains(&"first user line"));
        assert!(text.contains(&"second user line"));
        assert!(text.contains(&"first answer line"));
        assert!(text.contains(&""));
        assert!(text.contains(&"second answer line"));
    }
""",
)

append_test(
    "clarification conversation retention",
    "crates/medusa-tui/src/app/tests.rs",
    "fn clarification_question_and_confirmed_answer_stay_in_transcript",
    """#[test]
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
""",
)

for needle, paths in [
    (
        "assistant_narration_is_one_headline",
        ["crates/medusa-tui/src/runtime/tests.rs"],
    ),
    (
        "assistant_title(",
        ["crates/medusa-tui/src/runtime/support.rs"],
    ),
    (
        "draft.text.replace('\\n', \" / \")",
        [
            "crates/medusa-tui/src/render.rs",
            "crates/medusa-tui/src/render/support.rs",
        ],
    ),
]:
    for path in paths:
        if needle in Path(path).read_text():
            raise SystemExit(f"stale contract {needle!r} remains in {path}")

print("all transcript transforms applied")
