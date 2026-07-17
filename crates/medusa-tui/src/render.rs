pub(super) mod support;

use super::*;
pub(super) use support::*;

#[cfg(unix)]
pub(super) fn draw(
    stdout: &mut io::Stdout,
    _options: &TuiOptions,
    identity: &UiIdentity,
    app: &AppState,
    _jobs: &[JobRecord],
    _daemon_status: &str,
) -> io::Result<()> {
    draw_common(stdout, identity, app)
}

#[cfg(not(unix))]
#[allow(dead_code)]
pub(super) fn draw_portable(
    stdout: &mut io::Stdout,
    _options: &TuiOptions,
    identity: &UiIdentity,
    app: &AppState,
) -> io::Result<()> {
    let (width, height) = size()?;
    draw_frame(
        stdout,
        width,
        &render_frame(identity, app, width, height),
        None,
    )?;
    stdout.flush()
}

#[cfg(test)]
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct PortableRenderSnapshot {
    terminal_size: (u16, u16),
    status: String,
    transcript: Vec<TranscriptEntry>,
    plan: Option<app::TranscriptPlan>,
    token_count: u64,
    elapsed_seconds: Option<u64>,
    draft: PromptDraft,
    command_selection: usize,
    model_label: Option<String>,
    effort_label: Option<String>,
    plan_mode: bool,
    spinner_frame: u8,
    model_modal: Option<app::ModelModal>,
    welcome_visible: bool,
}

#[cfg(test)]
pub(super) fn portable_render_snapshot(
    app: &AppState,
    terminal_size: (u16, u16),
) -> PortableRenderSnapshot {
    PortableRenderSnapshot {
        terminal_size,
        status: app.status.clone(),
        transcript: app.transcript.clone(),
        plan: app.plan.clone(),
        token_count: app.token_count,
        elapsed_seconds: app.elapsed_seconds(),
        draft: app.composer.draft.clone(),
        command_selection: app.command_selection,
        model_label: app.model_label.clone(),
        effort_label: app.effort_label.clone(),
        plan_mode: app.plan_mode,
        spinner_frame: app.spinner_frame,
        model_modal: app.model_modal().cloned(),
        welcome_visible: app.welcome_visible(),
    }
}

pub(super) fn running_status(app: &AppState) -> String {
    format!(
        "{} ({} · ↑ {} tokens)",
        app.status,
        format_elapsed(app.elapsed_seconds().unwrap_or_default()),
        format_token_count(app.token_count)
    )
}

pub(super) fn format_elapsed(seconds: u64) -> String {
    let minutes = seconds / 60;
    if minutes == 0 {
        return format!("{seconds}s");
    }
    format!("{minutes}m {}s", seconds % 60)
}

pub(super) fn format_token_count(tokens: u64) -> String {
    if tokens < 1_000 {
        return tokens.to_string();
    }
    format!("{:.1}k", tokens as f64 / 1_000.0)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct UiIdentity {
    model: String,
    effort: String,
}

impl UiIdentity {
    pub(super) fn for_repo(repo: &Path) -> Self {
        let project = repo.join(".medusa/config.toml");
        let project = project.exists().then_some(project);
        let config =
            Config::load_layers(None, project.as_deref(), &BTreeMap::new(), &BTreeMap::new())
                .unwrap_or_default();
        Self {
            model: config.model.name,
            effort: effort_label(config.agent.max_turns).to_owned(),
        }
    }
}

pub(super) fn effort_label(max_turns: u32) -> &'static str {
    match max_turns {
        0..=99 => "effort:low",
        100..=299 => "effort:medium",
        _ => "effort:high",
    }
}

#[cfg(unix)]
pub(super) fn draw_common(
    stdout: &mut io::Stdout,
    identity: &UiIdentity,
    app: &AppState,
) -> io::Result<()> {
    let (width, height) = size()?;
    let frame = render_frame(identity, app, width, height);
    draw_frame(stdout, width, &frame, None)?;
    stdout.flush()
}

#[allow(dead_code)]
pub(super) fn legacy_draw_common(
    stdout: &mut io::Stdout,
    identity: &UiIdentity,
    app: &AppState,
) -> io::Result<()> {
    let (width, height) = size()?;
    queue!(
        stdout,
        MoveTo(0, 0),
        Clear(ClearType::CurrentLine),
        MoveTo(0, HEADER_TOP_PADDING)
    )?;
    for logo_line in MEDUSA_LOGO {
        print_styled_line(stdout, width, logo_line, Color::Cyan, Attribute::Bold)?;
    }
    queue!(
        stdout,
        Clear(ClearType::UntilNewLine),
        SetForegroundColor(Color::Magenta),
        SetAttribute(Attribute::Bold),
        Print(wrap_to_width(
            &format!(
                "{} {}",
                app.model_label.as_deref().unwrap_or(&identity.model),
                app.effort_label.as_deref().unwrap_or(&identity.effort)
            ),
            width
        )),
        SetAttribute(Attribute::Reset),
        ResetColor,
        Print("\r\n"),
    )?;
    let header_height = HEADER_TOP_PADDING + 4;
    let model_modal = app.model_modal();
    let modal_lines = model_modal.map(model_modal_lines).unwrap_or_default();
    let suggestions = if model_modal.is_none() {
        command_suggestions(&app.composer.draft.text)
    } else {
        Vec::new()
    };
    let available_suggestion_rows = height.saturating_sub(header_height.saturating_add(4));
    let visible_suggestions = suggestions
        .iter()
        .take(usize::from(available_suggestion_rows))
        .collect::<Vec<_>>();
    let requested_composer_height = if model_modal.is_some() {
        3_u16.saturating_add(u16::try_from(modal_lines.len()).unwrap_or(u16::MAX))
    } else {
        4_u16.saturating_add(u16::try_from(visible_suggestions.len()).unwrap_or(u16::MAX))
    };
    let composer_height = requested_composer_height.min(height.saturating_sub(header_height));
    let content_rows = height.saturating_sub(composer_height + header_height) as usize;
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
                        format!("  └ {}", attachment_label(attachment)),
                        Color::DarkGrey,
                    )
                }));
            }
            TranscriptEntry::Activity(activity) => lines.extend(activity_lines(activity)),
            TranscriptEntry::System(message) => lines.push(system_line(message)),
        }
    }
    if app.is_running() {
        lines.push(StyledLine::with_marker(
            spinner_marker(app.spinner_frame),
            Color::Magenta,
            running_status(app),
            Color::Grey,
        ));
    }
    if let Some(plan) = &app.plan {
        lines.extend(plan_lines(plan));
    }
    let visible_content = lines
        .iter()
        .rev()
        .take(content_rows)
        .rev()
        .collect::<Vec<_>>();
    for line in &visible_content {
        line.print(stdout, width)?;
    }
    for _ in visible_content.len()..content_rows {
        queue!(stdout, Clear(ClearType::UntilNewLine), Print("\r\n"))?;
    }

    let composer_top = height.saturating_sub(composer_height);
    queue!(
        stdout,
        MoveTo(0, composer_top),
        SetForegroundColor(Color::DarkGrey),
        Print("─".repeat(width as usize)),
        ResetColor,
        Print("\r\n")
    )?;
    if model_modal.is_some() {
        let available_modal_rows = composer_height.saturating_sub(3);
        for line in modal_lines.iter().take(usize::from(available_modal_rows)) {
            line.print(stdout, width)?;
        }
        print_separator(stdout, width)?;
        StyledLine::with_marker(
            "› ",
            Color::Magenta,
            "up/down choose · tab focus · enter set for this session · esc cancel",
            Color::DarkGrey,
        )
        .print(stdout, width)?;
        return stdout.flush();
    }
    for (index, suggestion) in visible_suggestions.iter().enumerate() {
        let selected = index == app.command_selection;
        StyledLine::with_marker(
            if selected { "> " } else { "  " },
            if selected {
                Color::Magenta
            } else {
                Color::DarkGrey
            },
            format!("{:<34} {}", suggestion.usage, suggestion.description),
            if selected { Color::White } else { Color::Grey },
        )
        .print(stdout, width)?;
    }
    let prompt = if app.composer.draft.text.is_empty() {
        "Describe a coding task...".to_owned()
    } else {
        composer_prompt_text(&app.composer.draft.text)
    };
    StyledLine::with_marker(
        "> ",
        Color::Cyan,
        prompt,
        if app.composer.draft.text.is_empty() {
            Color::DarkGrey
        } else {
            Color::White
        },
    )
    .print(stdout, width)?;
    print_separator(stdout, width)?;
    StyledLine::with_marker(
        "› ",
        Color::Magenta,
        if app.is_running() {
            "working · ctrl+c to interrupt · esc to exit"
        } else {
            "enter to submit · ctrl+v to paste · tab to complete commands · esc to exit"
        },
        Color::DarkGrey,
    )
    .print(stdout, width)?;
    stdout.flush()
}

pub(super) fn render_frame(
    identity: &UiIdentity,
    app: &AppState,
    width: u16,
    height: u16,
) -> Vec<StyledLine> {
    let blank = StyledLine::new("", Color::Reset);
    let mut frame = vec![blank.clone(); usize::from(height)];
    if app.welcome_visible() {
        render_loading_screen(&mut frame, width, height);
        return frame;
    }
    let mut row = usize::from(HEADER_TOP_PADDING);
    for logo_line in MEDUSA_LOGO {
        set_frame_line(&mut frame, row, StyledLine::new(logo_line, Color::Cyan));
        row = row.saturating_add(1);
    }
    set_frame_line(
        &mut frame,
        row,
        StyledLine::new(
            format!(
                "{} {}",
                app.model_label.as_deref().unwrap_or(&identity.model),
                app.effort_label.as_deref().unwrap_or(&identity.effort)
            ),
            Color::Magenta,
        ),
    );

    let header_height = HEADER_TOP_PADDING + 4;
    let question_modal = app.question_modal();
    let model_modal = app.model_modal();
    let modal_lines = question_modal
        .map(question_modal_lines)
        .or_else(|| model_modal.map(model_modal_lines))
        .unwrap_or_default();
    let is_modal = question_modal.is_some() || model_modal.is_some();
    let plan_panel = if !is_modal && app.task_list_visible {
        app.plan.as_ref().map(plan_lines).unwrap_or_default()
    } else {
        Vec::new()
    };
    let panel_rows = u16::try_from(plan_panel.len()).unwrap_or(u16::MAX);
    let base_composer_rows = 4_u16.saturating_add(panel_rows);
    let suggestions = if !is_modal {
        command_suggestions(&app.composer.draft.text)
    } else {
        Vec::new()
    };
    let available_suggestion_rows =
        height.saturating_sub(header_height.saturating_add(base_composer_rows));
    let visible_suggestions = suggestions
        .into_iter()
        .take(usize::from(available_suggestion_rows))
        .collect::<Vec<_>>();
    let requested_composer_height = if is_modal {
        3_u16.saturating_add(u16::try_from(modal_lines.len()).unwrap_or(u16::MAX))
    } else {
        base_composer_rows
            .saturating_add(u16::try_from(visible_suggestions.len()).unwrap_or(u16::MAX))
    };
    let composer_height = requested_composer_height.min(height.saturating_sub(header_height));
    let content_rows = usize::from(height.saturating_sub(composer_height + header_height));
    let mut content = transcript_lines(app);
    if app.is_running() {
        content.push(StyledLine::with_marker(
            spinner_marker(app.spinner_frame),
            Color::Magenta,
            running_status(app),
            Color::Grey,
        ));
    }
    let visible_content = content
        .iter()
        .rev()
        .take(content_rows)
        .rev()
        .collect::<Vec<_>>();
    let mut content_row = usize::from(header_height);
    for line in visible_content {
        set_frame_line(&mut frame, content_row, line.clone());
        content_row = content_row.saturating_add(1);
    }

    let mut bottom_row = usize::from(height.saturating_sub(composer_height));
    set_frame_line(&mut frame, bottom_row, separator_line(width));
    bottom_row = bottom_row.saturating_add(1);
    if is_modal {
        for line in modal_lines
            .into_iter()
            .take(usize::from(composer_height.saturating_sub(3)))
        {
            set_frame_line(&mut frame, bottom_row, line);
            bottom_row = bottom_row.saturating_add(1);
        }
        set_frame_line(&mut frame, bottom_row, separator_line(width));
        bottom_row = bottom_row.saturating_add(1);
        let help = if let Some(question_modal) = question_modal {
            if question_modal.is_reviewing() {
                "enter confirm and send - shift+tab edit answers"
            } else {
                "up/down choose - space multi-select - enter next - tab switch"
            }
        } else {
            "tab field - arrows choose - type or paste key - enter apply - esc cancel"
        };
        set_frame_line(
            &mut frame,
            bottom_row,
            StyledLine::with_marker("> ", Color::Magenta, help, Color::DarkGrey),
        );
        return frame;
    }

    for line in plan_panel {
        set_frame_line(&mut frame, bottom_row, line);
        bottom_row = bottom_row.saturating_add(1);
    }
    for (index, suggestion) in visible_suggestions.iter().enumerate() {
        let selected = index == app.command_selection;
        set_frame_line(
            &mut frame,
            bottom_row,
            StyledLine::with_marker(
                if selected { "> " } else { "  " },
                if selected {
                    Color::Magenta
                } else {
                    Color::DarkGrey
                },
                format!("{:<34} {}", suggestion.usage, suggestion.description),
                if selected { Color::White } else { Color::Grey },
            ),
        );
        bottom_row = bottom_row.saturating_add(1);
    }
    let prompt = if app.composer.draft.text.is_empty() {
        "Describe a coding task...".to_owned()
    } else {
        composer_prompt_text(&app.composer.draft.text)
    };
    set_frame_line(
        &mut frame,
        bottom_row,
        StyledLine::with_marker(
            "> ",
            Color::Cyan,
            prompt,
            if app.composer.draft.text.is_empty() {
                Color::DarkGrey
            } else {
                Color::White
            },
        ),
    );
    bottom_row = bottom_row.saturating_add(1);
    set_frame_line(&mut frame, bottom_row, separator_line(width));
    bottom_row = bottom_row.saturating_add(1);
    set_frame_line(
        &mut frame,
        bottom_row,
        StyledLine::with_marker(
            "> ",
            Color::Magenta,
            if app.is_running() {
                "working - ctrl+c interrupt - ctrl+t tasks"
            } else {
                "enter submit - ctrl+v paste - tab commands - ctrl+t tasks"
            },
            Color::DarkGrey,
        ),
    );
    frame
}
