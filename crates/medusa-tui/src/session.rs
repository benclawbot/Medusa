use super::*;
use crate::{
    daemon_status::DaemonMonitor,
    render::support::{app_error, runtime_error},
};
use std::time::Instant;

const DOUBLE_CTRL_C_WINDOW: Duration = Duration::from_secs(1);

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
        .unwrap_or_else(|| "current".to_owned());
    let mut app = AppState::new(
        options.repo.clone(),
        draft_key,
        options.initial_prompt.clone().unwrap_or_default(),
        clipboard,
    )?;
    let identity = UiIdentity::for_repo(&options.repo);
    let runtime = RuntimeController::start(options.repo.clone());
    let mut terminal = TerminalGuard::enter()?;
    run_loop(terminal.stdout(), &options, &identity, &mut app, &runtime)
}

struct TerminalGuard {
    stdout: io::Stdout,
    active: bool,
}

impl TerminalGuard {
    fn enter() -> io::Result<Self> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        if let Err(error) = execute!(stdout, EnterAlternateScreen, EnableBracketedPaste, Hide) {
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
    runtime: &RuntimeController,
) -> io::Result<ExitReason> {
    let mut daemon = DaemonMonitor::new(options.socket_path());
    let mut last_ctrl_c = None;
    loop {
        drain_runtime_events(app, runtime)?;
        app.tick();
        let daemon_snapshot = daemon.poll(app);
        draw(
            stdout,
            options,
            identity,
            app,
            &daemon_snapshot.jobs,
            &daemon_snapshot.status,
        )?;
        if event::poll(Duration::from_millis(100))? {
            let terminal_event = event::read()?;
            if app.dismiss_welcome_for_event(&terminal_event) {
                continue;
            }
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
            if ctrl_d_on_empty(&terminal_event, app) {
                return Ok(ExitReason::InputClosed);
            }
            if handle_app_action(app, runtime, terminal_event)? {
                return Ok(ExitReason::UserQuit);
            }
        }
        thread::sleep(Duration::from_millis(25));
    }
}

#[cfg(not(unix))]
pub(super) fn run_loop(
    stdout: &mut io::Stdout,
    options: &TuiOptions,
    identity: &UiIdentity,
    app: &mut AppState,
    runtime: &RuntimeController,
) -> io::Result<ExitReason> {
    let mut last_frame: Option<Vec<StyledLine>> = None;
    let mut last_ctrl_c = None;
    let mut daemon = DaemonMonitor::new(options.socket_path());
    loop {
        drain_runtime_events(app, runtime)?;
        app.tick();
        let _daemon_snapshot = daemon.poll(app);
        let (width, height) = size()?;
        let frame = render_frame(identity, app, width, height);
        if last_frame.as_ref() != Some(&frame) {
            draw_portable_frame(stdout, width, &frame, last_frame.as_deref())?;
            last_frame = Some(frame);
        }
        if event::poll(Duration::from_millis(100))? {
            let terminal_event = event::read()?;
            if app.dismiss_welcome_for_event(&terminal_event) {
                continue;
            }
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
            if ctrl_d_on_empty(&terminal_event, app) {
                return Ok(ExitReason::InputClosed);
            }
            if handle_app_action(app, runtime, terminal_event)? {
                return Ok(ExitReason::UserQuit);
            }
        }
        thread::sleep(Duration::from_millis(25));
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
    runtime: &RuntimeController,
    terminal_event: Event,
) -> io::Result<bool> {
    let action = app.handle_event(terminal_event).map_err(app_error)?;
    handle_action(app, runtime, action)
}

fn handle_action(
    app: &mut AppState,
    runtime: &RuntimeController,
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
                    app.transcript
                        .push(TranscriptEntry::System(format!("error: {error}")));
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
                    app.transcript
                        .push(TranscriptEntry::System(format!("error: {error}")));
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
                    app.transcript
                        .push(TranscriptEntry::System(format!("error: {error}")));
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
                    app.transcript
                        .push(TranscriptEntry::System(format!("error: {error}")));
                    app.status = "model configuration rejected".to_owned();
                }
            }
            Ok(false)
        }
        AppAction::None | AppAction::Redraw => Ok(false),
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
                app.record_activity(TranscriptActivity {
                    id: activity.id,
                    kind: match activity.kind {
                        RuntimeActivityKind::Assistant => TranscriptActivityKind::Assistant,
                        RuntimeActivityKind::Done => TranscriptActivityKind::Done,
                        RuntimeActivityKind::Error => TranscriptActivityKind::Error,
                        RuntimeActivityKind::Tool => TranscriptActivityKind::Tool,
                        RuntimeActivityKind::Verification => TranscriptActivityKind::Verification,
                    },
                    title: activity.title,
                    details: activity.details,
                });
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
                model_elapsed_millis,
            } => {
                app.record_usage(
                    input_tokens,
                    output_tokens,
                    cache_read_input_tokens,
                    cache_creation_input_tokens,
                    model_elapsed_millis,
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
            } => {
                app.set_runtime_settings(model, effort, plan_mode, credential_configured);
            }
            RuntimeEvent::Notice { title, details } => {
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
            RuntimeEvent::Completed { session_id } => {
                app.record_activity(TranscriptActivity {
                    id: None,
                    kind: TranscriptActivityKind::Done,
                    title: "Task completed".to_owned(),
                    details: vec![format!("session {session_id}")],
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
                app.record_activity(TranscriptActivity {
                    id: None,
                    kind: TranscriptActivityKind::Error,
                    title: "Task failed".to_owned(),
                    details: vec![error],
                });
                app.status = "agent failed".to_owned();
                app.finish_run();
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
    use crossterm::event::KeyEvent;

    fn key(code: KeyCode, modifiers: KeyModifiers) -> Event {
        Event::Key(KeyEvent::new(code, modifiers))
    }

    #[test]
    fn escape_interrupts_at_top_level_but_remains_available_to_modals() {
        let mut last_ctrl_c = None;
        assert_eq!(
            session_control_action(
                &key(KeyCode::Esc, KeyModifiers::NONE),
                false,
                &mut last_ctrl_c,
            ),
            Some(AppAction::Interrupt)
        );
        assert_eq!(
            session_control_action(
                &key(KeyCode::Esc, KeyModifiers::NONE),
                true,
                &mut last_ctrl_c,
            ),
            None
        );
    }

    #[test]
    fn second_ctrl_c_within_one_second_quits() {
        let mut last_ctrl_c = None;
        let ctrl_c = key(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(
            session_control_action(&ctrl_c, false, &mut last_ctrl_c),
            Some(AppAction::Interrupt)
        );
        assert!(last_ctrl_c.is_some());
        assert_eq!(
            session_control_action(&ctrl_c, false, &mut last_ctrl_c),
            Some(AppAction::Quit)
        );
        assert!(last_ctrl_c.is_none());
    }

    #[test]
    fn expired_ctrl_c_window_starts_a_new_interrupt_sequence() {
        let mut last_ctrl_c =
            Some(Instant::now() - DOUBLE_CTRL_C_WINDOW - Duration::from_millis(1));
        assert_eq!(
            session_control_action(
                &key(KeyCode::Char('c'), KeyModifiers::CONTROL),
                false,
                &mut last_ctrl_c,
            ),
            Some(AppAction::Interrupt)
        );
        assert!(last_ctrl_c.is_some());
    }

    #[test]
    fn another_key_resets_the_double_ctrl_c_window() {
        let mut last_ctrl_c = Some(Instant::now());
        assert_eq!(
            session_control_action(
                &key(KeyCode::Char('x'), KeyModifiers::NONE),
                false,
                &mut last_ctrl_c,
            ),
            None
        );
        assert!(last_ctrl_c.is_none());
    }
}
