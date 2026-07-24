pub(super) mod markdown;
pub(super) mod support;

use super::*;
use support::wrap_to_width;
pub(crate) use support::*;

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
    let frame = render_frame(identity, app, width, height);
    draw_portable_frame(stdout, width, &frame, None)
}

#[cfg(not(unix))]
pub(super) fn draw_portable_frame(
    stdout: &mut io::Stdout,
    width: u16,
    frame: &[StyledLine],
    previous: Option<&[StyledLine]>,
) -> io::Result<()> {
    draw_frame(stdout, width, frame, previous)?;
    stdout.flush()
}

#[cfg(test)]
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct PortableRenderSnapshot {
    terminal_size: (u16, u16),
    status: String,
    transcript: Vec<TranscriptEntry>,
    plan: Option<app::TranscriptPlan>,
    input_tokens: u64,
    output_tokens: u64,
    timed_output_tokens: u64,
    cache_read_input_tokens: u64,
    cache_creation_input_tokens: u64,
    current_context_tokens: u64,
    context_window_tokens: u64,
    auto_compact_percent: u8,
    model_elapsed_millis: u64,
    run_elapsed_seconds: Option<u64>,
    session_elapsed_seconds: u64,
    draft: PromptDraft,
    command_selection: usize,
    model_label: Option<String>,
    effort_label: Option<String>,
    plan_mode: bool,
    spinner_frame: u8,
    selection: Option<TextSelection>,
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
        input_tokens: app.input_tokens,
        output_tokens: app.output_tokens,
        timed_output_tokens: app.timed_output_tokens,
        cache_read_input_tokens: app.cache_read_input_tokens,
        cache_creation_input_tokens: app.cache_creation_input_tokens,
        current_context_tokens: app.current_context_tokens(),
        context_window_tokens: app.context_window_tokens(),
        auto_compact_percent: app.auto_compact_percent(),
        model_elapsed_millis: app.model_elapsed_millis,
        run_elapsed_seconds: app.elapsed_seconds(),
        session_elapsed_seconds: app.session_elapsed_seconds(),
        draft: app.composer.draft.clone(),
        command_selection: app.command_selection,
        model_label: app.model_label.clone(),
        effort_label: app.effort_label.clone(),
        plan_mode: app.plan_mode,
        spinner_frame: app.spinner_frame,
        selection: app.selection,
        model_modal: app.model_modal().cloned(),
        welcome_visible: app.welcome_visible(),
    }
}

pub(super) fn running_status(app: &AppState) -> String {
    format!(
        "{} ({} · turn {})",
        app.status,
        format_elapsed(app.elapsed_seconds().unwrap_or_default()),
        app.active_turn
    )
}

pub(super) fn session_metrics_line(app: &AppState) -> String {
    let rate = app
        .output_tokens_per_second()
        .map_or_else(|| "—".to_owned(), format_token_rate);
    format!(
        "session {} · total {} · input {} · output {} · cache-read {} · cache-write {} · cost {} · {} · {rate} tok/s",
        format_elapsed(app.session_elapsed_seconds()),
        format_token_count(app.total_tokens),
        format_token_count(app.input_tokens),
        format_token_count(app.output_tokens),
        format_token_count(app.cache_read_input_tokens),
        format_token_count(app.cache_creation_input_tokens),
        format_cost(app.estimated_cost_microusd),
        app.usage_provenance.as_deref().unwrap_or("—"),
    )
}

pub(super) fn context_meter_line(app: &AppState) -> String {
    const SEGMENTS: u64 = 10;
    let window = app.context_window_tokens();
    let used = app.current_context_tokens().min(window);
    let percent = if window == 0 {
        0
    } else {
        used.saturating_mul(100) / window
    };
    let filled = if window == 0 {
        0
    } else {
        used.saturating_mul(SEGMENTS) / window
    };
    let bar = format!(
        "{}{}",
        "█".repeat(usize::try_from(filled).unwrap_or(usize::MAX)),
        "░".repeat(usize::try_from(SEGMENTS.saturating_sub(filled)).unwrap_or_default())
    );
    format!(
        "context [{bar}] {}/{} ({percent}%) · auto-compact {}%",
        format_token_count(used),
        format_token_count(window),
        app.auto_compact_percent(),
    )
}

fn format_cost(microusd: u64) -> String {
    if microusd == 0 {
        return "—".to_owned();
    }
    format!("${:.4}", microusd as f64 / 1_000_000.0)
}

fn format_token_rate(tokens_per_second: f64) -> String {
    if tokens_per_second < 1_000.0 {
        return format!("{tokens_per_second:.1}");
    }
    format!("{:.1}k", tokens_per_second / 1_000.0)
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
    if tokens < 1_000_000 {
        return format!("{:.1}k", tokens as f64 / 1_000.0);
    }
    format!("{:.1}m", tokens as f64 / 1_000_000.0)
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
    StyledLine::new(session_metrics_line(app), Color::DarkGrey).print(stdout, width)?;
    let header_height = HEADER_TOP_PADDING + 5;
    let model_modal = app.model_modal();
    let modal_lines = model_modal.map(model_modal_lines).unwrap_or_default();
    let suggestions = if model_modal.is_none() {
        command_suggestions(&app.composer.draft.text, app.repository())
    } else {
        Vec::new()
    };
    let available_suggestion_rows = height.saturating_sub(header_height.saturating_add(5));
    let suggestion_rows = usize::from(available_suggestion_rows);
    let suggestion_start = app
        .command_selection
        .saturating_sub(suggestion_rows.saturating_sub(1))
        .min(suggestions.len().saturating_sub(suggestion_rows));
    let visible_suggestions = suggestions
        .iter()
        .skip(suggestion_start)
        .take(suggestion_rows)
        .collect::<Vec<_>>();
    let requested_composer_height = if model_modal.is_some() {
        3_u16.saturating_add(u16::try_from(modal_lines.len()).unwrap_or(u16::MAX))
    } else {
        5_u16.saturating_add(u16::try_from(visible_suggestions.len()).unwrap_or(u16::MAX))
    };
    let composer_height = requested_composer_height.min(height.saturating_sub(header_height));
    let content_rows = height.saturating_sub(composer_height + header_height) as usize;
    let mut lines = transcript_lines(app, width);
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
        .skip(
            app.scrollback_offset()
                .min(lines.len().saturating_sub(content_rows)),
        )
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
        let selected = suggestion_start + index == app.command_selection;
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
        if app.is_running() {
            "Add a follow-up for the next turn...".to_owned()
        } else {
            "Describe a coding task...".to_owned()
        }
    } else {
        composer_prompt_text(&app.composer.draft.text)
    };
    StyledLine::with_marker(
        "> ",
        Color::Cyan,
        format!("{USER_INPUT_INDENT}{prompt}"),
        if app.composer.draft.text.is_empty() {
            Color::DarkGrey
        } else {
            Color::White
        },
    )
    .print(stdout, width)?;
    StyledLine::new(context_meter_line(app), Color::Grey).print(stdout, width)?;
    print_separator(stdout, width)?;
    StyledLine::with_marker(
        "› ",
        Color::Magenta,
        if app.is_running() {
            "enter queues a follow-up · ctrl+c interrupt · esc exit"
        } else {
            "enter selects/submits · ctrl+v paste · tab also completes commands · esc exit"
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
    row = row.saturating_add(1);
    set_frame_line(
        &mut frame,
        row,
        StyledLine::new(session_metrics_line(app), Color::DarkGrey),
    );

    let header_height = HEADER_TOP_PADDING + 5;
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
    let base_composer_rows = 5_u16.saturating_add(panel_rows);
    let suggestions = if !is_modal {
        command_suggestions(&app.composer.draft.text, app.repository())
    } else {
        Vec::new()
    };
    let available_suggestion_rows =
        height.saturating_sub(header_height.saturating_add(base_composer_rows));
    let suggestion_rows = usize::from(available_suggestion_rows);
    let suggestion_start = app
        .command_selection
        .saturating_sub(suggestion_rows.saturating_sub(1))
        .min(suggestions.len().saturating_sub(suggestion_rows));
    let visible_suggestions = suggestions
        .into_iter()
        .skip(suggestion_start)
        .take(suggestion_rows)
        .collect::<Vec<_>>();
    let requested_composer_height = if is_modal {
        3_u16.saturating_add(u16::try_from(modal_lines.len()).unwrap_or(u16::MAX))
    } else {
        base_composer_rows
            .saturating_add(u16::try_from(visible_suggestions.len()).unwrap_or(u16::MAX))
    };
    let composer_height = requested_composer_height.min(height.saturating_sub(header_height));
    let content_rows = usize::from(height.saturating_sub(composer_height + header_height));
    let mut content = transcript_lines(app, width);
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
        .skip(
            app.scrollback_offset()
                .min(content.len().saturating_sub(content_rows)),
        )
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
        apply_selection(&mut frame, app.selection);
        return frame;
    }

    for line in plan_panel {
        set_frame_line(&mut frame, bottom_row, line);
        bottom_row = bottom_row.saturating_add(1);
    }
    for (index, suggestion) in visible_suggestions.iter().enumerate() {
        let selected = suggestion_start + index == app.command_selection;
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
        if app.is_running() {
            "Add a follow-up for the next turn...".to_owned()
        } else {
            "Describe a coding task...".to_owned()
        }
    } else {
        composer_prompt_text(&app.composer.draft.text)
    };
    set_frame_line(
        &mut frame,
        bottom_row,
        StyledLine::with_marker(
            "> ",
            Color::Cyan,
            format!("{USER_INPUT_INDENT}{prompt}"),
            if app.composer.draft.text.is_empty() {
                Color::DarkGrey
            } else {
                Color::White
            },
        ),
    );
    bottom_row = bottom_row.saturating_add(1);
    set_frame_line(
        &mut frame,
        bottom_row,
        StyledLine::new(context_meter_line(app), Color::Grey),
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
                "enter queue follow-up - ctrl+c interrupt - ctrl+t tasks"
            } else {
                "enter submit - ctrl+v paste - tab commands - ctrl+t tasks"
            },
            Color::DarkGrey,
        ),
    );
    apply_selection(&mut frame, app.selection);
    frame
}

fn apply_selection(frame: &mut [StyledLine], selection: Option<TextSelection>) {
    let Some(selection) = selection else {
        return;
    };
    if selection.is_empty() {
        return;
    }
    let (start, end) = selection.ordered();
    for row in start.row..=end.row {
        let from = if row == start.row {
            usize::from(start.column)
        } else {
            0
        };
        let to = if row == end.row {
            usize::from(end.column).saturating_add(1)
        } else {
            usize::MAX
        };
        if let Some(line) = frame.get_mut(usize::from(row)) {
            line.set_selection(from, to);
        }
    }
}

pub(super) fn selected_text(frame: &[StyledLine], width: u16, selection: TextSelection) -> String {
    if selection.is_empty() {
        return String::new();
    }
    let (start, end) = selection.ordered();
    let mut text = String::new();
    for row in start.row..=end.row {
        if row != start.row {
            text.push('\n');
        }
        let Some(line) = frame.get(usize::from(row)) else {
            continue;
        };
        let chars = line.visible_text(width).chars().collect::<Vec<_>>();
        let from = if row == start.row {
            usize::from(start.column).min(chars.len())
        } else {
            0
        };
        let to = if row == end.row {
            usize::from(end.column).saturating_add(1).min(chars.len())
        } else {
            chars.len()
        };
        if from < to {
            text.extend(chars[from..to].iter());
        }
    }
    text
}
