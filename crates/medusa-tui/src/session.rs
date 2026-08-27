use super::*;
use crate::{
    daemon_status::DaemonMonitor,
    render::support::{app_error, runtime_error},
    runtime::RuntimeActivity,
};
use std::time::Instant;

const DOUBLE_CTRL_C_WINDOW: Duration = Duration::from_secs(1);
const INPUT_POLL_INTERVAL: Duration = Duration::from_millis(16);
const DAEMON_POLL_INTERVAL: Duration = Duration::from_millis(250);

pub fn run(options: TuiOptions) -> io::Result<ExitReason> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "interactive Medusa requires a TTY; use `medusa run` for headless execution",
        ));
    }

    let clipboard: Arc<dyn ClipboardService> = NativeClipboard::new()
        .map(|service| Arc::new(service) as Arc<dyn ClipboardService>)
        .unwrap_or_else(|_| Arc::new(UnsupportedClipboard));
    let draft_key = options
        .resume_session
        .clone()
        .or_else(|| {
            options
                .continue_latest
                .then(|| "continue-latest".to_owned())
        })
        .unwrap_or_else(|| "current".to_owned());
    let mut app = AppState::new(
        options.repo.clone(),
        draft_key,
        options.initial_prompt.clone().unwrap_or_default(),
        clipboard,
    )?;
    let identity = UiIdentity::for_repo(&options.repo);
    let mut runtime = runtime_for_options(&options).map_err(runtime_error)?;
    let mut terminal = TerminalGuard::enter()?;
    run_loop(
        terminal.stdout(),
        &options,
        &identity,
        &mut app,
        &mut runtime,
    )
}

fn runtime_for_options(
    options: &TuiOptions,
) -> Result<RuntimeController, crate::runtime::RuntimeError> {
    if let Some(session_id) = options.resume_session.as_deref() {
        return RuntimeController::start_resumed(options.repo.clone(), session_id);
    }
    if options.continue_latest {
        return RuntimeController::start_continue_latest(options.repo.clone());
    }
    Ok(RuntimeController::start(options.repo.clone()))
}

struct TerminalGuard {
    stdout: io::Stdout,
    active: bool,
}

impl TerminalGuard {
    fn enter() -> io::Result<Self> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        if let Err(error) = execute!(
            stdout,
            EnterAlternateScreen,
            EnableBracketedPaste,
            EnableMouseCapture,
            Hide
        ) {
            let _ = disable_raw_mode();
            return Err(error);
        }
        Ok(Self {
            stdout,
            active: true,
        })
    }

    fn stdout(&mut self) -> &mut io::Stdout {
        &mut self.stdout
    }

    fn restore(&mut self) {
        if !self.active {
            return;
        }
        let _ = disable_raw_mode();
        let _ = execute!(
            self.stdout,
            DisableBracketedPaste,
            DisableMouseCapture,
            Show,
            LeaveAlternateScreen
        );
        self.active = false;
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        self.restore();
    }
}

#[cfg(unix)]
pub(super) fn run_loop(
    stdout: &mut io::Stdout,
    options: &TuiOptions,
    identity: &UiIdentity,
    app: &mut AppState,
    runtime: &mut RuntimeController,
) -> io::Result<ExitReason> {
    let _ = runtime.ensure_daemon();
    let mut daemon = DaemonMonitor::new(options.socket_path());
    let (mut daemon_jobs, mut daemon_status) = daemon.poll(app);
    let mut next_daemon_poll = Instant::now() + DAEMON_POLL_INTERVAL;
    let mut last_ctrl_c = None;

    loop {
        drain_runtime_events(app, runtime)?;
        app.tick();

        let now = Instant::now();
        if now >= next_daemon_poll {
            (daemon_jobs, daemon_status) = daemon.poll(app);
            next_daemon_poll = now + DAEMON_POLL_INTERVAL;
        }

        draw(stdout, options, identity, app, &daemon_jobs, &daemon_status)?;
        if event::poll(INPUT_POLL_INTERVAL)? {
            let terminal_event = event::read()?;
            app.dismiss_welcome_for_event(&terminal_event);
            let modal_open = app.model_modal().is_some() || app.question_modal().is_some();
            if let Some(action) =
                session_control_action(&terminal_event, modal_open, &mut last_ctrl_c)
            {
                if handle_action(app, runtime, action)? {
                    return Ok(ExitReason::UserQuit);
                }
                continue;
            }
            if ctrl_l_redraw(&terminal_event) {
                continue;
            }
            if handle_mouse_selection(app, identity, &terminal_event)? {
                continue;
            }
            if ctrl_d_on_empty(&terminal_event, app) {
                return Ok(ExitReason::InputClosed);
            }
            if handle_app_action(app, runtime, terminal_event)? {
                return Ok(ExitReason::UserQuit);
            }
        }
    }
}

#[cfg(not(unix))]
pub(super) fn run_loop(
    stdout: &mut io::Stdout,
    options: &TuiOptions,
    identity: &UiIdentity,
    app: &mut AppState,
    runtime: &mut RuntimeController,
) -> io::Result<ExitReason> {
    let mut last_frame: Option<Vec<StyledLine>> = None;
    let mut last_ctrl_c = None;
    let _ = runtime.ensure_daemon();
    let mut daemon = DaemonMonitor::new(options.socket_path());
    let _ = daemon.poll(app);
    let mut next_daemon_poll = Instant::now() + DAEMON_POLL_INTERVAL;

    loop {
        drain_runtime_events(app, runtime)?;
        app.tick();

        let now = Instant::now();
        if now >= next_daemon_poll {
            let _ = daemon.poll(app);
            next_daemon_poll = now + DAEMON_POLL_INTERVAL;
        }

        let (width, height) = size()?;
        let frame = render_frame(identity, app, width, height);
        if last_frame.as_ref() != Some(&frame) {
            draw_portable_frame(stdout, width, &frame, last_frame.as_deref())?;
            last_frame = Some(frame);
        }
        if event::poll(INPUT_POLL_INTERVAL)? {
            let terminal_event = event::read()?;
            app.dismiss_welcome_for_event(&terminal_event);
            let modal_open = app.model_modal().is_some() || app.question_modal().is_some();
            if let Some(action) =
                session_control_action(&terminal_event, modal_open, &mut last_ctrl_c)
            {
                if handle_action(app, runtime, action)? {
                    return Ok(ExitReason::UserQuit);
                }
                continue;
            }
            if matches!(terminal_event, Event::Resize(_, _)) {
                last_frame = None;
            }
            if ctrl_l_redraw(&terminal_event) {
                last_frame = None;
                continue;
            }
            if handle_mouse_selection(app, identity, &terminal_event)? {
                last_frame = None;
                continue;
            }
            if ctrl_d_on_empty(&terminal_event, app) {
                return Ok(ExitReason::InputClosed);
            }
            if handle_app_action(app, runtime, terminal_event)? {
                return Ok(ExitReason::UserQuit);
            }
        }
    }
}

fn handle_mouse_selection(
    app: &mut AppState,
    identity: &UiIdentity,
    event: &Event,
) -> io::Result<bool> {
    let Event::Mouse(mouse) = event else {
        return Ok(false);
    };
    let position = TerminalPosition {
        row: mouse.row,
        column: mouse.column,
    };
    match mouse.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            app.begin_text_selection(position);
            Ok(true)
        }
        MouseEventKind::Drag(MouseButton::Left) => {
            app.update_text_selection(position);
            Ok(true)
        }
        MouseEventKind::Moved if app.is_selecting_text() => {
            app.update_text_selection(position);
            Ok(true)
        }
        MouseEventKind::Up(MouseButton::Left) => {
            let Some(selection) = app.finish_text_selection(position) else {
                return Ok(true);
            };
            if selection.is_empty() {
                return Ok(true);
            }
            let (width, height) = size()?;
            let frame = render_frame(identity, app, width, height);
            let text = selected_text(&frame, width, selection);
            if !text.is_empty()
                && let Err(error) = app.copy_text(&text)
            {
                app.status = format!("copy failed: {error}");
            }
            Ok(true)
        }
        MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => Ok(false),
        _ => Ok(false),
    }
}

fn session_control_action(
    terminal_event: &Event,
    modal_open: bool,
    last_ctrl_c: &mut Option<Instant>,
) -> Option<AppAction> {
    let Event::Key(key) = terminal_event else {
        return None;
    };
    if key.kind != KeyEventKind::Press {
        return None;
    }
    if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
        let now = Instant::now();
        if last_ctrl_c
            .take()
            .is_some_and(|previous| now.saturating_duration_since(previous) <= DOUBLE_CTRL_C_WINDOW)
        {
            return Some(AppAction::Quit);
        }
        *last_ctrl_c = Some(now);
        return Some(AppAction::Interrupt);
    }

    *last_ctrl_c = None;
    if key.code == KeyCode::Esc && !modal_open {
        return Some(AppAction::Interrupt);
    }
    None
}

pub(super) fn handle_app_action(
    app: &mut AppState,
    runtime: &mut RuntimeController,
    terminal_event: Event,
) -> io::Result<bool> {
    let action = app.handle_event(terminal_event).map_err(app_error)?;
    handle_action(app, runtime, action)
}

fn handle_action(
    app: &mut AppState,
    runtime: &mut RuntimeController,
    action: AppAction,
) -> io::Result<bool> {
    match action {
        AppAction::Quit => Ok(true),
        AppAction::Interrupt => {
            app.status = if runtime.cancel() {
                "cancellation requested".to_owned()
            } else {
                "no running task to cancel".to_owned()
            };
            Ok(false)
        }
        AppAction::Submit(draft) => {
            if should_resume_latest(app, &draft) {
                match RuntimeController::start_continue_latest(app.repository().to_path_buf()) {
                    Ok(resumed) => {
                        *runtime = resumed;
                        app.status = "resuming latest task".to_owned();
                    }
                    Err(error) => {
                        app.restore_rejected_submission(draft)?;
                        app.transcript.push(TranscriptEntry::System(format!(
                            "error: could not resume latest task: {}",
                            user_visible_runtime_error(&error.to_string())
                        )));
                        app.status = "resume unavailable; draft restored".to_owned();
                        return Ok(false);
                    }
                }
            }

            let bytes = draft.text.len();
            let attachments = draft.attachments.len();
            match runtime.submit(draft.clone()) {
                Ok(SubmitDisposition::Started) => {
                    app.status =
                        format!("running prompt: {bytes} bytes, {attachments} attachment(s)");
                }
                Ok(SubmitDisposition::Queued) => {
                    app.status = "follow-up queued for the next agent turn".to_owned();
                }
                Err(error) => {
                    app.restore_rejected_submission(draft)?;
                    app.transcript.push(TranscriptEntry::System(format!(
                        "error: {}",
                        user_visible_runtime_error(&error.to_string())
                    )));
                    app.status = "submission rejected; draft restored".to_owned();
                }
            }
            Ok(false)
        }
        AppAction::AnswerQuestion(answer) => {
            let draft = PromptDraft {
                text: answer,
                ..PromptDraft::default()
            };
            match runtime.submit(draft) {
                Ok(_) => {
                    app.status = "continuing with your answer".to_owned();
                }
                Err(error) => {
                    app.transcript.push(TranscriptEntry::System(format!(
                        "error: {}",
                        user_visible_runtime_error(&error.to_string())
                    )));
                    app.status = "answer rejected".to_owned();
                }
            }
            Ok(false)
        }
        AppAction::Command(command) => {
            match runtime.run_command(command) {
                Ok(()) => {
                    app.status = "command running".to_owned();
                }
                Err(error) => {
                    app.transcript.push(TranscriptEntry::System(format!(
                        "error: {}",
                        user_visible_runtime_error(&error.to_string())
                    )));
                    app.status = "command rejected".to_owned();
                }
            }
            Ok(false)
        }
        AppAction::ConfigureModel(configuration) => {
            match runtime.configure_model(configuration) {
                Ok(()) => {
                    app.status = "updating model configuration".to_owned();
                }
                Err(error) => {
                    app.transcript.push(TranscriptEntry::System(format!(
                        "error: {}",
                        user_visible_runtime_error(&error.to_string())
                    )));
                    app.status = "model configuration rejected".to_owned();
                }
            }
            Ok(false)
        }
        AppAction::None | AppAction::Redraw => Ok(false),
    }
}

fn should_resume_latest(app: &AppState, draft: &PromptDraft) -> bool {
    if !draft.attachments.is_empty() || !is_continuation_intent(&draft.text) {
        return false;
    }

    let mut user_messages = 0_usize;
    let mut has_assistant_message = false;
    for entry in &app.transcript {
        match entry {
            TranscriptEntry::User(_) => user_messages = user_messages.saturating_add(1),
            TranscriptEntry::Assistant(_) => has_assistant_message = true,
            _ => {}
        }
    }
    user_messages <= 1 && !has_assistant_message
}

fn is_continuation_intent(input: &str) -> bool {
    let normalized = input
        .trim()
        .trim_matches(|character: char| character.is_ascii_punctuation())
        .trim()
        .to_ascii_lowercase();
    matches!(
        normalized.as_str(),
        "go"
            | "go on"
            | "continue"
            | "continue on"
            | "proceed"
            | "resume"
            | "finish"
            | "finish it"
    )
}

fn is_internal_notice(title: &str) -> bool {
    let normalized = title.trim().to_ascii_lowercase();
    matches!(
        normalized.as_str(),
        "recovery available"
            | "recovery completed"
            | "background daemon recovered"
            | "checkpoint created"
            | "background daemon started"
            | "background daemon connected"
            | "model configuration updated"
            | "runtime capabilities"
            | "waiting for model or tool response"
    ) || normalized.starts_with("configuration revision ")
}

fn is_internal_activity_title(title: &str) -> bool {
    let normalized = title.trim().to_ascii_lowercase();
    normalized == "provider execution"
        || normalized == "checkpoint created"
        || normalized == "model response received"
        || normalized == "provider attempt classified"
        || normalized.starts_with("requesting ")
        || normalized.starts_with("waiting for ")
        || normalized.starts_with("codex ")
}

fn is_user_visible_activity(activity: &RuntimeActivity) -> bool {
    !matches!(
        activity.kind,
        RuntimeActivityKind::Assistant | RuntimeActivityKind::Tool
    ) && !is_internal_activity_title(&activity.title)
}

fn user_visible_runtime_error(error: &str) -> String {
    let visible = error
        .lines()
        .filter(|line| {
            let normalized = line.trim_start().to_ascii_lowercase();
            !normalized.starts_with("recovery:") && !normalized.starts_with("checkpoint:")
        })
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_owned();
    if visible.is_empty() {
        "operation failed; Medusa kept the latest durable state".to_owned()
    } else {
        visible
    }
}

pub(super) fn drain_runtime_events(
    app: &mut AppState,
    runtime: &RuntimeController,
) -> io::Result<()> {
    while let Some(event) = runtime.try_event().map_err(runtime_error)? {
        match event {
            RuntimeEvent::Started => {
                app.begin_run();
            }
            RuntimeEvent::AssistantText(text) => {
                app.record_assistant_text(text);
            }
            RuntimeEvent::Activity(activity) => {
                if activity.id.as_deref() == Some("runtime-capabilities")
                    || !is_user_visible_activity(&activity)
                {
                    continue;
                }
                app.record_activity(TranscriptActivity {
                    id: activity.id,
                    kind: match activity.kind {
                        RuntimeActivityKind::Assistant => TranscriptActivityKind::Assistant,
                        RuntimeActivityKind::Done => TranscriptActivityKind::Done,
                        RuntimeActivityKind::Error => TranscriptActivityKind::Error,
                        RuntimeActivityKind::Progress => TranscriptActivityKind::Progress,
                        RuntimeActivityKind::Tool => TranscriptActivityKind::Tool,
                        RuntimeActivityKind::Verification => TranscriptActivityKind::Verification,
                    },
                    title: activity.title,
                    details: activity.details,
                });
            }
            RuntimeEvent::Team(snapshot) => {
                let has_workers = !snapshot.workers.is_empty();
                for worker in snapshot.workers {
                    app.record_activity(TranscriptActivity {
                        id: Some(format!("team:{}", worker.worker_id)),
                        kind: match worker.lifecycle {
                            medusa_runtime::TeamWorkerLifecycle::Completed
                            | medusa_runtime::TeamWorkerLifecycle::Integrated => {
                                TranscriptActivityKind::Done
                            }
                            medusa_runtime::TeamWorkerLifecycle::Failed => {
                                TranscriptActivityKind::Error
                            }
                            _ => TranscriptActivityKind::Progress,
                        },
                        title: format!(
                            "{} · {} · {:?}",
                            worker.worker_id, worker.task_id, worker.lifecycle
                        ),
                        details: vec![
                            format!("role {}", worker.role),
                            format!("turn {}", worker.turn),
                            format!(
                                "session {}",
                                worker.session_id.as_deref().unwrap_or("pending")
                            ),
                            worker.last_update,
                        ],
                    });
                }
                if has_workers {
                    app.status = if snapshot.active {
                        "team active".to_owned()
                    } else {
                        "team complete".to_owned()
                    };
                }
            }
            RuntimeEvent::Plan(plan) => {
                app.set_plan(plan);
            }
            RuntimeEvent::Question(question) => {
                app.open_question(question.questions);
            }
            RuntimeEvent::Usage {
                input_tokens,
                output_tokens,
                cache_read_input_tokens,
                cache_creation_input_tokens,
                total_tokens,
                duration_ms,
                tokens_per_second_milli,
                estimated_cost_microusd,
                provenance,
            } => {
                app.record_turn_usage(
                    input_tokens,
                    output_tokens,
                    cache_read_input_tokens,
                    cache_creation_input_tokens,
                    total_tokens,
                    duration_ms,
                    tokens_per_second_milli,
                    estimated_cost_microusd,
                    provenance,
                );
            }
            RuntimeEvent::Progress { turn } => {
                app.update_turn(turn);
            }
            RuntimeEvent::Settings {
                model,
                effort,
                plan_mode,
                credential_configured,
                context_window_tokens,
                auto_compact_percent,
            } => {
                app.set_runtime_settings(
                    model,
                    effort,
                    plan_mode,
                    credential_configured,
                    context_window_tokens,
                    auto_compact_percent,
                );
            }
            RuntimeEvent::Notice { title, details } => {
                if is_internal_notice(&title) {
                    continue;
                }
                let status = title.to_ascii_lowercase();
                app.record_activity(TranscriptActivity {
                    id: None,
                    kind: TranscriptActivityKind::Progress,
                    title,
                    details,
                });
                app.status = status;
            }
            RuntimeEvent::NewSession => {
                app.clear_for_new_session();
            }
            RuntimeEvent::Compacted { message } => {
                app.compact_transcript(message);
            }
            RuntimeEvent::Completed { session_id: _ } => {
                app.record_activity(TranscriptActivity {
                    id: None,
                    kind: TranscriptActivityKind::Done,
                    title: "Task completed".to_owned(),
                    details: Vec::new(),
                });
                app.status = "completed".to_owned();
                app.finish_run();
            }
            RuntimeEvent::TurnFinished => {
                app.status = "ready".to_owned();
                app.finish_run();
            }
            RuntimeEvent::Cancelled => {
                app.record_activity(TranscriptActivity {
                    id: None,
                    kind: TranscriptActivityKind::Done,
                    title: "Task cancelled".to_owned(),
                    details: Vec::new(),
                });
                app.status = "cancelled".to_owned();
                app.finish_run();
            }
            RuntimeEvent::Failed(error) => {
                let retry_draft = if app.composer.draft.text.is_empty()
                    && app.composer.draft.attachments.is_empty()
                {
                    app.transcript.iter().rev().find_map(|entry| match entry {
                        TranscriptEntry::User(draft) => Some(draft.clone()),
                        _ => None,
                    })
                } else {
                    None
                };
                app.record_activity(TranscriptActivity {
                    id: None,
                    kind: TranscriptActivityKind::Error,
                    title: "Task failed".to_owned(),
                    details: vec![user_visible_runtime_error(&error)],
                });
                app.status = if retry_draft.is_some() {
                    "agent failed; draft restored".to_owned()
                } else {
                    "agent failed".to_owned()
                };
                app.finish_run();
                if let Some(draft) = retry_draft {
                    app.restore_failed_submission(draft)?;
                }
            }
        }
    }
    Ok(())
}

pub(super) fn ctrl_d_on_empty(event: &Event, app: &AppState) -> bool {
    matches!(
        event,
        Event::Key(key)
            if key.kind == KeyEventKind::Press
                && key.code == KeyCode::Char('d')
                && key.modifiers.contains(KeyModifiers::CONTROL)
                && app.composer.draft.text.is_empty()
                && app.composer.draft.attachments.is_empty()
    )
}

pub(super) fn ctrl_l_redraw(event: &Event) -> bool {
    matches!(
        event,
        Event::Key(key)
            if key.kind == KeyEventKind::Press
                && key.code == KeyCode::Char('l')
                && key.modifiers.contains(KeyModifiers::CONTROL)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn continuation_intents_are_exact_and_conservative() {
        for input in [
            "go",
            "GO",
            "go on",
            "continue",
            "continue on",
            "proceed",
            "resume",
            "finish",
            "finish it",
            " continue! ",
        ] {
            assert!(is_continuation_intent(input), "{input}");
        }
        for input in [
            "go fix tests",
            "continue with a new feature",
            "resume issue 12",
            "finished",
            "",
        ] {
            assert!(!is_continuation_intent(input), "{input}");
        }
    }

    #[test]
    fn continuation_only_reconnects_without_prior_conversation() {
        let directory = tempfile::tempdir().expect("tempdir");
        let mut app = AppState::new(
            directory.path().to_path_buf(),
            "continuation-intent",
            "",
            Arc::new(UnsupportedClipboard),
        )
        .expect("app");
        let draft = PromptDraft {
            text: "go".to_owned(),
            ..PromptDraft::default()
        };
        app.transcript.push(TranscriptEntry::User(draft.clone()));
        assert!(should_resume_latest(&app, &draft));
        app.transcript.push(TranscriptEntry::Assistant("done".to_owned()));
        assert!(!should_resume_latest(&app, &draft));
    }

    #[test]
    fn internal_recovery_notices_are_hidden() {
        assert!(is_internal_notice("Recovery available"));
        assert!(is_internal_notice("Recovery completed"));
        assert!(is_internal_notice("Background daemon recovered"));
        assert!(is_internal_notice("Checkpoint created"));
        assert!(!is_internal_notice("Recovery action failed closed"));
        assert!(!is_internal_notice("Checkpoint restore failed"));
        assert!(!is_internal_notice("Configuration updated"));
    }

    #[test]
    fn routine_runtime_diagnostics_are_hidden_from_the_transcript() {
        for title in [
            "Model configuration updated",
            "Runtime capabilities",
            "Waiting for model or tool response",
            "Configuration revision 4 applied",
            "Background daemon connected",
        ] {
            assert!(is_internal_notice(title), "{title}");
        }
        for title in [
            "Requesting openai-oauth/gpt-5.6-luna",
            "Waiting for model or tool response",
        ] {
            assert!(is_internal_activity_title(title), "{title}");
        }
        assert!(!is_internal_activity_title("Inspect repository"));
    }

    #[test]
    fn model_and_primary_tool_activities_are_hidden_but_failures_remain_visible() {
        for (kind, title) in [
            (RuntimeActivityKind::Assistant, "Model response received"),
            (RuntimeActivityKind::Tool, "Shell(cargo test)"),
            (RuntimeActivityKind::Done, "Checkpoint created"),
        ] {
            assert!(!is_user_visible_activity(&RuntimeActivity {
                id: None,
                kind,
                title: title.to_owned(),
                details: Vec::new(),
            }));
        }
        assert!(is_user_visible_activity(&RuntimeActivity {
            id: None,
            kind: RuntimeActivityKind::Error,
            title: "Shell(cargo test) failed".to_owned(),
            details: vec!["exit code: 1".to_owned()],
        }));
    }

    #[test]
    fn recovery_bookkeeping_is_removed_from_failures() {
        let visible = user_visible_runtime_error(
            "tool failed\nRecovery: cp-123; session abc\ncheckpoint: cp-123",
        );
        assert_eq!(visible, "tool failed");
        assert_eq!(
            user_visible_runtime_error("Recovery: cp-123"),
            "operation failed; Medusa kept the latest durable state"
        );
        assert_eq!(
            user_visible_runtime_error(
                "checkpoint cursor is beyond the canonical journal while creating its payload"
            ),
            "checkpoint cursor is beyond the canonical journal while creating its payload"
        );
    }
}
