#!/usr/bin/env python3
from pathlib import Path

app_path = Path("crates/medusa-tui/src/app.rs")
app = app_path.read_text()
app = app.replace(
'''use std::{
    io,
''',
'''use std::{
    collections::BTreeSet,
    io,
''',
1,
)

scrollback_anchor = '''#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Scrollback {
'''
key_definition = '''#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum ActivityDetailKey {
    Id(String),
    TranscriptIndex(usize),
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Scrollback {
'''
if scrollback_anchor not in app:
    raise SystemExit("scrollback anchor not found")
app = app.replace(scrollback_anchor, key_definition, 1)

app = app.replace(
'''    pub task_list_visible: bool,
    pub activity_details_expanded: bool,
    pub spinner_frame: u8,
''',
'''    pub task_list_visible: bool,
    expanded_activity_details: BTreeSet<ActivityDetailKey>,
    pub spinner_frame: u8,
''',
1,
)
app = app.replace(
'''            task_list_visible: true,
            activity_details_expanded: false,
            spinner_frame: 0,
''',
'''            task_list_visible: true,
            expanded_activity_details: BTreeSet::new(),
            spinner_frame: 0,
''',
1,
)

method_anchor = '''    pub fn scrollback_scroll_down(&mut self, step: usize) {
        self.scrollback.scroll_down(step);
    }

    pub fn new(
'''
methods = '''    pub fn scrollback_scroll_down(&mut self, step: usize) {
        self.scrollback.scroll_down(step);
    }

    fn activity_detail_key(index: usize, activity: &TranscriptActivity) -> ActivityDetailKey {
        activity.id.as_ref().map_or(
            ActivityDetailKey::TranscriptIndex(index),
            |id| ActivityDetailKey::Id(id.clone()),
        )
    }

    #[must_use]
    pub(crate) fn activity_details_expanded(
        &self,
        index: usize,
        activity: &TranscriptActivity,
    ) -> bool {
        self.expanded_activity_details
            .contains(&Self::activity_detail_key(index, activity))
    }

    pub(crate) fn activity_detail_expansion_snapshot(&self) -> Vec<bool> {
        self.transcript
            .iter()
            .enumerate()
            .map(|(index, entry)| match entry {
                TranscriptEntry::Activity(activity) => {
                    self.activity_details_expanded(index, activity)
                }
                _ => false,
            })
            .collect()
    }

    fn toggle_latest_activity_details(&mut self) {
        let key = self
            .transcript
            .iter()
            .enumerate()
            .rev()
            .find_map(|(index, entry)| match entry {
                TranscriptEntry::Activity(activity) if !activity.details.is_empty() => {
                    Some(Self::activity_detail_key(index, activity))
                }
                _ => None,
            });
        let Some(key) = key else {
            self.status = "no activity details available".to_owned();
            return;
        };
        if !self.expanded_activity_details.remove(&key) {
            self.expanded_activity_details.insert(key);
        }
    }

    pub fn new(
'''
if method_anchor not in app:
    raise SystemExit("method anchor not found")
app = app.replace(method_anchor, methods, 1)

app = app.replace(
'''            if key.code == KeyCode::Char('e') && key.modifiers.contains(KeyModifiers::CONTROL) {
                self.activity_details_expanded = !self.activity_details_expanded;
                return Ok(AppAction::Redraw);
            }
''',
'''            if key.code == KeyCode::Char('e') && key.modifiers.contains(KeyModifiers::CONTROL) {
                self.toggle_latest_activity_details();
                return Ok(AppAction::Redraw);
            }
''',
1,
)
app_path.write_text(app)

render_path = Path("crates/medusa-tui/src/render.rs")
render = render_path.read_text()
render = render.replace(
'''    plan_mode: bool,
    spinner_frame: u8,
''',
'''    plan_mode: bool,
    activity_detail_expansion: Vec<bool>,
    spinner_frame: u8,
''',
1,
)
render = render.replace(
'''        plan_mode: app.plan_mode,
        spinner_frame: app.spinner_frame,
''',
'''        plan_mode: app.plan_mode,
        activity_detail_expansion: app.activity_detail_expansion_snapshot(),
        spinner_frame: app.spinner_frame,
''',
1,
)
render_path.write_text(render)

support_path = Path("crates/medusa-tui/src/render/support.rs")
support = support_path.read_text()
support = support.replace(
'''    for entry in &app.transcript {
        match entry {
''',
'''    for (entry_index, entry) in app.transcript.iter().enumerate() {
        match entry {
''',
1,
)
support = support.replace(
'''                lines.extend(activity_lines(activity, app.activity_details_expanded));
''',
'''                lines.extend(activity_lines(
                    activity,
                    app.activity_details_expanded(entry_index, activity),
                ));
''',
1,
)
support_path.write_text(support)

lib_path = Path("crates/medusa-tui/src/lib.rs")
lib = lib_path.read_text()
anchor = '''    #[test]
    fn spinner_changes_only_one_retained_frame_row() {
'''
test = '''    #[test]
    fn ctrl_e_expands_only_the_latest_activity_with_details() {
        let directory = tempfile::tempdir().expect("tempdir");
        let mut app = AppState::new(
            directory.path().to_path_buf(),
            "per-activity-details",
            "",
            Arc::new(UnsupportedClipboard),
        )
        .expect("app");
        app.dismiss_welcome_for_event(&Event::Paste(String::new()));
        app.transcript.extend([
            TranscriptEntry::Activity(TranscriptActivity {
                id: Some("first".to_owned()),
                kind: TranscriptActivityKind::Verification,
                title: "First verification".to_owned(),
                details: (1..=8).map(|index| format!("first {index}")).collect(),
            }),
            TranscriptEntry::Activity(TranscriptActivity {
                id: Some("second".to_owned()),
                kind: TranscriptActivityKind::Verification,
                title: "Second verification".to_owned(),
                details: (1..=8).map(|index| format!("second {index}")).collect(),
            }),
        ]);

        let ctrl_e = Event::Key(crossterm::event::KeyEvent::new(
            KeyCode::Char('e'),
            KeyModifiers::CONTROL,
        ));
        assert!(matches!(app.handle_event(ctrl_e.clone()).expect("expand"), AppAction::Redraw));
        assert!(!app.activity_details_expanded(
            0,
            match &app.transcript[0] {
                TranscriptEntry::Activity(activity) => activity,
                _ => unreachable!(),
            },
        ));
        assert!(app.activity_details_expanded(
            1,
            match &app.transcript[1] {
                TranscriptEntry::Activity(activity) => activity,
                _ => unreachable!(),
            },
        ));

        app.transcript.push(TranscriptEntry::Activity(TranscriptActivity {
            id: None,
            kind: TranscriptActivityKind::Progress,
            title: "Legacy activity".to_owned(),
            details: vec!["legacy detail".to_owned()],
        }));
        assert!(matches!(app.handle_event(ctrl_e).expect("expand legacy"), AppAction::Redraw));
        assert!(app.activity_details_expanded(
            2,
            match &app.transcript[2] {
                TranscriptEntry::Activity(activity) => activity,
                _ => unreachable!(),
            },
        ));
    }

    #[test]
    fn spinner_changes_only_one_retained_frame_row() {
'''
if anchor not in lib:
    raise SystemExit("test anchor not found")
lib = lib.replace(anchor, test, 1)
lib_path.write_text(lib)
