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

use std::thread;

use app::{
    AppAction, AppError, AppState, TerminalPosition, TextSelection, TranscriptActivity,
    TranscriptActivityKind, TranscriptEntry,
};
use clipboard::{ClipboardService, PromptAttachment, PromptDraft, UnsupportedClipboard};
use commands::command_suggestions;
use crossterm::{
    cursor::{Hide, MoveTo, Show},
    event::{
        self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
        Event, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind,
    },
    execute, queue,
    style::{
        Attribute, Color, Print, ResetColor, SetAttribute, SetBackgroundColor, SetForegroundColor,
    },
    terminal::{
        Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode,
        enable_raw_mode, size,
    },
};
use medusa_config::Config;
#[cfg(unix)]
use medusa_daemon::JobRecord;
use native_clipboard::NativeClipboard;
use runtime::{RuntimeActivityKind, RuntimeController, RuntimeEvent, SubmitDisposition};

const MEDUSA_LOGO: [&str; 3] = [
    "╭┬╮╭─╴╶┬╮╷ ╷╭─╮╭─╮",
    "│││├╴  │││ │╰─╮├─┤",
    "╵ ╵╰─╴╶┴╯╰─╯╰─╯╵ ╵",
];
const MEDUSA_LOADING_LOGO: &str = include_str!("medusa_logo_ascii.txt");
const HEADER_TOP_PADDING: u16 = 1;
const USER_INPUT_INDENT: &str = "    ";

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

mod daemon_status;
// Renderer helpers intentionally keep explicit style parameters, and renderer tests compare
// exact row membership. Keep these allowances scoped to the private presentation module.
#[allow(clippy::manual_contains, clippy::too_many_arguments)]
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
        app.spinner_frame = 1;
        let before = render_frame(&UiIdentity::for_repo(directory.path()), &app, 80, 24);
        app.spinner_frame = 2;
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
    fn running_status_and_header_metrics_use_real_accounting() {
        let directory = tempfile::tempdir().expect("tempdir");
        let mut app = AppState::new(
            directory.path().to_path_buf(),
            "status-test",
            "",
            Arc::new(UnsupportedClipboard),
        )
        .expect("app");
        app.begin_run();
        app.update_turn(3);
        app.record_usage(0, 300, 0, 0, 0);
        app.record_usage(700, 1_200, 200, 100, 2_000);
        assert_eq!(running_status(&app), "Working (0s · turn 3)");
        assert_eq!(
            session_metrics_line(&app),
            "session 0s · total 2.5k · input 700 · output 1.5k · cache-read 200 · cache-write 100 · cost — · estimated · 600.0 tok/s"
        );
    }

    #[test]
    fn authoritative_usage_renders_cost_rate_and_provider_provenance() {
        let directory = tempfile::tempdir().expect("tempdir");
        let mut app = AppState::new(
            directory.path().to_path_buf(),
            "authoritative-usage",
            "",
            Arc::new(UnsupportedClipboard),
        )
        .expect("app");
        app.record_turn_usage(
            1_000,
            500,
            100,
            50,
            1_650,
            2_000,
            825_000,
            12_345,
            "provider".to_owned(),
        );
        assert_eq!(
            session_metrics_line(&app),
            "session 0s · total 1.6k · input 1.0k · output 500 · cache-read 100 · cache-write 50 · cost $0.0123 · provider · 825.0 tok/s"
        );
    }

    #[test]
    fn context_meter_shows_current_window_use_and_progress() {
        let directory = tempfile::tempdir().expect("tempdir");
        let mut app = AppState::new(
            directory.path().to_path_buf(),
            "context-meter",
            "",
            Arc::new(UnsupportedClipboard),
        )
        .expect("app");
        app.set_runtime_settings(
            "minimax / MiniMax-M3".to_owned(),
            "effort:high".to_owned(),
            false,
            true,
            1_000_000,
            40,
        );
        app.record_usage(50_000, 1_000, 350_000, 0, 1_000);
        app.dismiss_welcome_for_event(&Event::Paste(String::new()));

        assert_eq!(app.current_context_tokens(), 400_000);
        assert_eq!(
            context_meter_line(&app),
            "context [████░░░░░░] 400.0k/1.0m (40%) · auto-compact 40%"
        );
        let frame = render_frame(&UiIdentity::for_repo(directory.path()), &app, 100, 24);
        assert!(
            frame
                .iter()
                .rev()
                .take(5)
                .any(|line| line.text.contains("400.0k/1.0m"))
        );
    }

    #[test]
    fn context_use_tracks_the_latest_request_instead_of_session_sum() {
        let directory = tempfile::tempdir().expect("tempdir");
        let mut app = AppState::new(
            directory.path().to_path_buf(),
            "context-current",
            "",
            Arc::new(UnsupportedClipboard),
        )
        .expect("app");
        app.record_usage(10, 1, 20, 30, 1);
        app.record_usage(100, 1, 200, 300, 1);

        assert_eq!(app.current_context_tokens(), 600);
        assert_eq!(app.total_input_tokens(), 660);
    }

    #[test]
    fn new_session_resets_all_usage_totals() {
        let directory = tempfile::tempdir().expect("tempdir");
        let mut app = AppState::new(
            directory.path().to_path_buf(),
            "metrics-reset",
            "",
            Arc::new(UnsupportedClipboard),
        )
        .expect("app");
        app.record_usage(10, 20, 30, 40, 500);
        app.clear_for_new_session();
        assert_eq!(app.total_input_tokens(), 0);
        assert_eq!(app.output_tokens, 0);
        assert_eq!(app.timed_output_tokens, 0);
        assert_eq!(app.cache_read_input_tokens, 0);
        assert_eq!(app.cache_creation_input_tokens, 0);
        assert_eq!(app.model_elapsed_millis, 0);
        assert_eq!(app.output_tokens_per_second(), None);
    }

    #[test]
    fn effort_label_tracks_turn_budget() {
        assert_eq!(effort_label(50), "effort:low");
        assert_eq!(effort_label(100), "effort:medium");
        assert_eq!(effort_label(500), "effort:high");
    }

    #[test]
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
}
