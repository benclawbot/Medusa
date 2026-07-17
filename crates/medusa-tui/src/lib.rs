pub mod app;
pub mod clipboard;
pub mod commands;
pub mod draft_store;
pub mod input;
pub mod native_clipboard;
pub mod runtime;

use std::{
    collections::BTreeMap,
    io::{self, IsTerminal, Write},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

#[cfg(unix)]
use std::thread;

use app::{
    AppAction, AppError, AppState, TranscriptActivity, TranscriptActivityKind, TranscriptEntry,
};
use clipboard::{ClipboardService, PromptAttachment, PromptDraft, UnsupportedClipboard};
use commands::command_suggestions;
use crossterm::{
    cursor::{Hide, MoveTo, Show},
    event::{
        self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode, KeyEventKind,
        KeyModifiers,
    },
    execute, queue,
    style::{Attribute, Color, Print, ResetColor, SetAttribute, SetForegroundColor},
    terminal::{
        Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode,
        enable_raw_mode, size,
    },
};
use medusa_config::Config;
use native_clipboard::NativeClipboard;
use runtime::{RuntimeActivityKind, RuntimeController, RuntimeEvent};

const MEDUSA_LOGO: [&str; 3] = [
    "╭┬╮╭─╴╶┬╮╷ ╷╭─╮╭─╮",
    "│││├╴  │││ │╰─╮├─┤",
    "╵ ╵╰─╴╶┴╯╰─╯╰─╯╵ ╵",
];
const MEDUSA_LOADING_LOGO: &str = include_str!("medusa_logo_ascii.txt");
const HEADER_TOP_PADDING: u16 = 1;

#[cfg(unix)]
use medusa_daemon::{DaemonClient, JobRecord, Request, Response};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TuiOptions {
    pub repo: PathBuf,
    pub socket: Option<PathBuf>,
    pub initial_prompt: Option<String>,
    pub resume_session: Option<String>,
    pub continue_latest: bool,
}

impl TuiOptions {
    #[must_use]
    pub fn for_repo(repo: impl Into<PathBuf>) -> Self {
        Self {
            repo: repo.into(),
            socket: None,
            initial_prompt: None,
            resume_session: None,
            continue_latest: false,
        }
    }

    #[must_use]
    pub fn socket_path(&self) -> PathBuf {
        self.socket
            .clone()
            .unwrap_or_else(|| self.repo.join(".medusa/daemon/medusa.sock"))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExitReason {
    UserQuit,
    InputClosed,
}

mod render;
mod session;

use render::*;
pub use session::run;

/// Render `value` to a string that fits within `width` terminal columns.
#[must_use]
pub fn wrap_to_width(value: &str, width: u16) -> String {
    render::support::wrap_to_width(value, width)
}

#[cfg(test)]
use session::*;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clipboard::ImageAttachment;

    #[test]
    fn default_socket_is_repository_scoped() {
        let options = TuiOptions::for_repo("/tmp/example");
        assert_eq!(
            options.socket_path(),
            PathBuf::from("/tmp/example/.medusa/daemon/medusa.sock")
        );
    }

    #[test]
    fn explicit_socket_wins() {
        let mut options = TuiOptions::for_repo("/tmp/example");
        options.socket = Some(PathBuf::from("/tmp/medusa.sock"));
        assert_eq!(options.socket_path(), PathBuf::from("/tmp/medusa.sock"));
    }

    #[test]
    fn ctrl_l_requests_a_terminal_redraw() {
        assert!(ctrl_l_redraw(&Event::Key(crossterm::event::KeyEvent::new(
            KeyCode::Char('l'),
            KeyModifiers::CONTROL,
        ))));
    }

    #[test]
    fn image_attachment_label_includes_dimensions() {
        let attachment = PromptAttachment::Image(ImageAttachment {
            display_name: "shot.png".to_owned(),
            width: 10,
            height: 20,
            rgba: vec![0; 8],
            source_format: Some("image/rgba8".to_owned()),
        });
        assert!(attachment_label(&attachment).contains("10x20"));
    }

    #[test]
    fn portable_render_snapshot_changes_only_with_visible_state() {
        let directory = tempfile::tempdir().expect("tempdir");
        let mut app = AppState::new(
            directory.path().to_path_buf(),
            "redraw-test",
            "",
            Arc::new(UnsupportedClipboard),
        )
        .expect("app");

        let initial = portable_render_snapshot(&app, (80, 24));
        assert_eq!(initial, portable_render_snapshot(&app, (80, 24)));

        app.status = "agent running".to_owned();
        assert_ne!(initial, portable_render_snapshot(&app, (80, 24)));

        app.begin_run();
        let running = portable_render_snapshot(&app, (80, 24));
        app.tick();
        assert_ne!(running, portable_render_snapshot(&app, (80, 24)));
    }

    #[test]
    fn loading_logo_is_aligned_and_first_input_only_dismisses_it() {
        let directory = tempfile::tempdir().expect("tempdir");
        let mut app = AppState::new(
            directory.path().to_path_buf(),
            "loading-logo",
            "identify",
            Arc::new(UnsupportedClipboard),
        )
        .expect("app");

        let initial = render_frame(&UiIdentity::for_repo(directory.path()), &app, 80, 40);
        assert!(initial.iter().any(|line| line.text.contains("@@@@@@@@@@")));
        let first_logo_line = initial
            .iter()
            .find(|line| line.text.contains(":-++**+=:"))
            .expect("first logo line");
        assert_eq!(first_logo_line.text.find(":-++**+=:"), Some(29));
        assert!(
            initial
                .iter()
                .any(|line| line.text.contains("Start typing to begin"))
        );

        let enter = Event::Key(crossterm::event::KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE,
        ));
        assert!(app.dismiss_welcome_for_event(&enter));
        assert_eq!(app.composer.draft.text, "identify");
        assert!(app.transcript.is_empty());

        let after_input = render_frame(&UiIdentity::for_repo(directory.path()), &app, 80, 40);
        assert!(
            !after_input
                .iter()
                .any(|line| line.text.contains("@@@@@@@@@@"))
        );
        assert!(
            after_input
                .iter()
                .any(|line| line.text.contains("identify"))
        );
    }

    #[test]
    fn question_and_plan_are_rendered_in_the_bottom_panels() {
        let directory = tempfile::tempdir().expect("tempdir");
        let mut app = AppState::new(
            directory.path().to_path_buf(),
            "panel-test",
            "",
            Arc::new(UnsupportedClipboard),
        )
        .expect("app");
        app.dismiss_welcome_for_event(&Event::Paste(String::new()));
        app.set_plan(app::TranscriptPlan {
            steps: vec![app::TranscriptPlanStep {
                title: "Inspect the repository".to_owned(),
                state: app::TranscriptPlanStepState::Active,
            }],
        });
        let plan_frame = render_frame(&UiIdentity::for_repo(directory.path()), &app, 80, 24);
        assert!(
            plan_frame
                .iter()
                .rev()
                .take(6)
                .any(|line| { line.text.contains("Inspect the repository") })
        );

        app.open_question(vec![app::QuestionPrompt {
            header: "Project".to_owned(),
            question: "Which project should I use?".to_owned(),
            options: vec![app::QuestionOption {
                label: "Projects/site-a".to_owned(),
                description: "Use the existing site".to_owned(),
            }],
            multi_select: false,
        }]);
        let question_frame = render_frame(&UiIdentity::for_repo(directory.path()), &app, 80, 24);
        assert!(
            question_frame
                .iter()
                .rev()
                .take(8)
                .any(|line| { line.text.contains("Which project should I use?") })
        );
        assert!(
            !question_frame
                .iter()
                .any(|line| { line.text.contains("Describe a coding task") })
        );
    }

    #[test]
    fn model_form_renders_effort_key_and_explicit_apply_action() {
        let directory = tempfile::tempdir().expect("tempdir");
        let mut app = AppState::new(
            directory.path().to_path_buf(),
            "model-form",
            "/model",
            Arc::new(UnsupportedClipboard),
        )
        .expect("app");
        assert!(matches!(
            app.handle_event(Event::Key(crossterm::event::KeyEvent::new(
                KeyCode::Enter,
                KeyModifiers::NONE,
            )))
            .expect("open model form"),
            AppAction::Redraw
        ));

        let frame = render_frame(&UiIdentity::for_repo(directory.path()), &app, 80, 24);
        assert!(frame.iter().any(|line| line.text == "Model configuration"));
        assert!(
            frame
                .iter()
                .any(|line| line.text.contains("Effort    high"))
        );
        assert!(frame.iter().any(|line| line.text == "Apply configuration"));
    }

    #[test]
    fn tool_and_assistant_activities_render_without_detail_rows() {
        for kind in [
            TranscriptActivityKind::Tool,
            TranscriptActivityKind::Assistant,
        ] {
            let lines = activity_lines(&TranscriptActivity {
                id: None,
                kind,
                title: "High-level step".to_owned(),
                details: vec!["argument: private detail".to_owned()],
            });
            assert_eq!(lines.len(), 1);
            assert_eq!(lines[0].text, "High-level step");
        }
    }

    #[test]
    fn spinner_changes_only_one_retained_frame_row() {
        let directory = tempfile::tempdir().expect("tempdir");
        let mut app = AppState::new(
            directory.path().to_path_buf(),
            "render-diff",
            "",
            Arc::new(UnsupportedClipboard),
        )
        .expect("app");
        app.dismiss_welcome_for_event(&Event::Paste(String::new()));
        app.begin_run();
        app.tick();
        let before = render_frame(&UiIdentity::for_repo(directory.path()), &app, 80, 24);
        app.tick();
        let after = render_frame(&UiIdentity::for_repo(directory.path()), &app, 80, 24);
        assert_eq!(
            before
                .iter()
                .zip(after.iter())
                .filter(|(left, right)| left != right)
                .count(),
            1
        );
    }

    #[test]
    fn running_status_includes_elapsed_time_and_tokens() {
        let directory = tempfile::tempdir().expect("tempdir");
        let mut app = AppState::new(
            directory.path().to_path_buf(),
            "status-test",
            "",
            Arc::new(UnsupportedClipboard),
        )
        .expect("app");
        app.begin_run();
        app.add_output_tokens(1_200);
        assert_eq!(running_status(&app), "Working (0s · ↑ 1.2k tokens)");
    }

    #[test]
    fn effort_label_tracks_turn_budget() {
        assert_eq!(effort_label(50), "effort:low");
        assert_eq!(effort_label(100), "effort:medium");
        assert_eq!(effort_label(500), "effort:high");
    }
}
