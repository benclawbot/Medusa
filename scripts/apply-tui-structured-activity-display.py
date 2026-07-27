#!/usr/bin/env python3
from pathlib import Path

support_path = Path("crates/medusa-tui/src/render/support.rs")
support = support_path.read_text()

old_transcript = '''pub(crate) fn transcript_lines(app: &AppState, width: u16) -> Vec<StyledLine> {
    let mut lines = Vec::new();
    for entry in &app.transcript {
        match entry {
            TranscriptEntry::User(draft) => {
'''
new_transcript = '''#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ActivityGroup {
    Execution,
    Verification,
}

fn activity_group(activity: &TranscriptActivity) -> ActivityGroup {
    if matches!(activity.kind, TranscriptActivityKind::Verification) {
        ActivityGroup::Verification
    } else {
        ActivityGroup::Execution
    }
}

fn activity_group_heading(group: ActivityGroup) -> StyledLine {
    match group {
        ActivityGroup::Execution => StyledLine::new("Execution activity", Color::DarkYellow),
        ActivityGroup::Verification => StyledLine::new("Verification evidence", Color::Blue),
    }
}

pub(crate) fn transcript_lines(app: &AppState, width: u16) -> Vec<StyledLine> {
    let mut lines = Vec::new();
    let mut previous_activity_group = None;
    for entry in &app.transcript {
        match entry {
            TranscriptEntry::User(draft) => {
                previous_activity_group = None;
'''
if old_transcript not in support:
    raise SystemExit("transcript anchor not found")
support = support.replace(old_transcript, new_transcript, 1)

support = support.replace(
'''            TranscriptEntry::Assistant(text) => lines.extend(
                super::markdown::markdown_block_lines("Medusa  ", Color::Magenta, text, width),
            ),
            TranscriptEntry::Activity(activity) => {
                lines.extend(activity_lines(activity, app.activity_details_expanded));
            }
            TranscriptEntry::System(message) => lines.push(system_line(message)),
''',
'''            TranscriptEntry::Assistant(text) => {
                previous_activity_group = None;
                lines.extend(super::markdown::markdown_block_lines(
                    "Medusa  ",
                    Color::Magenta,
                    text,
                    width,
                ));
            }
            TranscriptEntry::Activity(activity) => {
                let group = activity_group(activity);
                if previous_activity_group != Some(group) {
                    lines.push(activity_group_heading(group));
                    previous_activity_group = Some(group);
                }
                lines.extend(activity_lines(activity, app.activity_details_expanded));
            }
            TranscriptEntry::System(message) => {
                previous_activity_group = None;
                lines.push(system_line(message));
            }
''',
1,
)

old_activity = '''pub(crate) fn activity_lines(activity: &TranscriptActivity, expanded: bool) -> Vec<StyledLine> {
    let color = match activity.kind {
        TranscriptActivityKind::Assistant => Color::Green,
        TranscriptActivityKind::Done => Color::Green,
        TranscriptActivityKind::Error => Color::Red,
        TranscriptActivityKind::Progress => Color::Yellow,
        TranscriptActivityKind::Tool => Color::Green,
        TranscriptActivityKind::Verification => Color::Blue,
    };
    let foreground = if matches!(
        activity.kind,
        TranscriptActivityKind::Assistant
            | TranscriptActivityKind::Error
            | TranscriptActivityKind::Tool
    ) {
        Color::White
    } else {
        Color::Grey
    };
    let marker = if matches!(activity.kind, TranscriptActivityKind::Error) {
        "✻"
    } else {
        "●"
    };
    let mut lines = vec![StyledLine::with_marker(
        format!("{marker} "),
        color,
        &activity.title,
        foreground,
    )];
'''
new_activity = '''pub(crate) fn activity_lines(activity: &TranscriptActivity, expanded: bool) -> Vec<StyledLine> {
    let color = match activity.kind {
        TranscriptActivityKind::Assistant => Color::Green,
        TranscriptActivityKind::Done => Color::Green,
        TranscriptActivityKind::Error => Color::Red,
        TranscriptActivityKind::Progress => Color::Yellow,
        TranscriptActivityKind::Tool => Color::Green,
        TranscriptActivityKind::Verification => Color::Blue,
    };
    let foreground = if matches!(
        activity.kind,
        TranscriptActivityKind::Assistant
            | TranscriptActivityKind::Error
            | TranscriptActivityKind::Tool
    ) {
        Color::White
    } else {
        Color::Grey
    };
    let (marker, lifecycle) = match activity.kind {
        TranscriptActivityKind::Done => ("✓", "succeeded"),
        TranscriptActivityKind::Error => ("✻", "failed"),
        TranscriptActivityKind::Verification => ("◇", "verified"),
        TranscriptActivityKind::Assistant
        | TranscriptActivityKind::Progress
        | TranscriptActivityKind::Tool => ("●", "running"),
    };
    let mut lines = vec![StyledLine::with_marker(
        format!("{marker} "),
        color,
        format!("[{lifecycle}] {}", activity.title),
        foreground,
    )];
'''
if old_activity not in support:
    raise SystemExit("activity anchor not found")
support = support.replace(old_activity, new_activity, 1)
support_path.write_text(support)

lib_path = Path("crates/medusa-tui/src/lib.rs")
lib = lib_path.read_text()
lib = lib.replace(
'            assert_eq!(lines[0].text, "High-level step");',
'            assert_eq!(lines[0].text, "[running] High-level step");',
1,
)

anchor = '''    #[test]
    fn spinner_changes_only_one_retained_frame_row() {
'''
new_test = '''    #[test]
    fn structured_activity_groups_and_lifecycle_labels_render() {
        let directory = tempfile::tempdir().expect("tempdir");
        let mut app = AppState::new(
            directory.path().to_path_buf(),
            "structured-activity",
            "",
            Arc::new(UnsupportedClipboard),
        )
        .expect("app");
        app.transcript.extend([
            TranscriptEntry::Activity(TranscriptActivity {
                id: Some("run".to_owned()),
                kind: TranscriptActivityKind::Progress,
                title: "Inspect repository".to_owned(),
                details: vec![],
            }),
            TranscriptEntry::Activity(TranscriptActivity {
                id: Some("done".to_owned()),
                kind: TranscriptActivityKind::Done,
                title: "Patch applied".to_owned(),
                details: vec![],
            }),
            TranscriptEntry::Activity(TranscriptActivity {
                id: Some("verify".to_owned()),
                kind: TranscriptActivityKind::Verification,
                title: "Focused tests passed".to_owned(),
                details: vec!["cargo test -p medusa-tui".to_owned()],
            }),
        ]);

        let lines = transcript_lines(&app, 100);
        let text = lines.iter().map(|line| line.text.as_str()).collect::<Vec<_>>();
        assert!(text.contains(&"Execution activity"));
        assert!(text.contains(&"[running] Inspect repository"));
        assert!(text.contains(&"[succeeded] Patch applied"));
        assert!(text.contains(&"Verification evidence"));
        assert!(text.contains(&"[verified] Focused tests passed"));
    }

    #[test]
    fn spinner_changes_only_one_retained_frame_row() {
'''
if anchor not in lib:
    raise SystemExit("test anchor not found")
lib = lib.replace(anchor, new_test, 1)
lib_path.write_text(lib)
