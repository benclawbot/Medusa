use super::*;

pub(super) fn render_loading_screen(frame: &mut [StyledLine], width: u16, height: u16) {
    let logo = MEDUSA_LOADING_LOGO
        .trim_matches(['\r', '\n'])
        .lines()
        .collect::<Vec<_>>();
    let block_width = logo
        .iter()
        .map(|line| line.chars().count())
        .max()
        .unwrap_or_default();
    let available_rows = usize::from(height.saturating_sub(2));
    let visible_rows = logo.len().min(available_rows);
    let first_line = logo.len().saturating_sub(visible_rows) / 2;
    let first_row = available_rows.saturating_sub(visible_rows) / 2;

    for (offset, line) in logo.iter().skip(first_line).take(visible_rows).enumerate() {
        set_frame_line(
            frame,
            first_row.saturating_add(offset),
            StyledLine::new(center_or_crop(line, block_width, width), Color::Cyan),
        );
    }

    if height > 0 {
        let hint = "Start typing to begin";
        set_frame_line(
            frame,
            usize::from(height.saturating_sub(1)),
            StyledLine::new(
                center_or_crop(hint, hint.chars().count(), width),
                Color::DarkGrey,
            ),
        );
    }
}

pub(super) fn center_or_crop(line: &str, block_width: usize, width: u16) -> String {
    let width = usize::from(width);
    if width >= block_width {
        return format!("{}{}", " ".repeat((width - block_width) / 2), line);
    }

    line.chars()
        .skip((block_width - width) / 2)
        .take(width)
        .collect()
}

pub(crate) fn transcript_lines(app: &AppState, width: u16) -> Vec<StyledLine> {
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
}

pub(super) fn set_frame_line(frame: &mut [StyledLine], row: usize, line: StyledLine) {
    if let Some(slot) = frame.get_mut(row) {
        *slot = line;
    }
}

pub(super) fn separator_line(width: u16) -> StyledLine {
    StyledLine::new("-".repeat(usize::from(width)), Color::DarkGrey)
}

pub(super) fn draw_frame(
    stdout: &mut io::Stdout,
    width: u16,
    frame: &[StyledLine],
    previous: Option<&[StyledLine]>,
) -> io::Result<()> {
    for (row, line) in frame.iter().enumerate() {
        if previous.is_some_and(|previous| previous.get(row) == Some(line)) {
            continue;
        }
        line.print_at(stdout, width, u16::try_from(row).unwrap_or(u16::MAX))?;
    }
    Ok(())
}

pub(super) fn spinner_marker(frame: u8) -> &'static str {
    match frame % 4 {
        0 => ". ",
        1 => "o ",
        2 => "O ",
        _ => "o ",
    }
}

pub(super) fn model_modal_lines(model_modal: &app::ModelModal) -> Vec<StyledLine> {
    use app::ModelModalFocus::{ApiKey, Apply, Effort, Model, Provider};

    let focus = model_modal.focus();
    let mut lines = vec![StyledLine::new("Model configuration", Color::Cyan)];
    lines.push(StyledLine::with_marker(
        if focus == Provider { "› " } else { "  " },
        if focus == Provider {
            Color::Magenta
        } else {
            Color::DarkGrey
        },
        format!("Provider  {}", model_modal.provider()),
        if focus == Provider {
            Color::White
        } else {
            Color::Grey
        },
    ));
    lines.push(StyledLine::with_marker(
        if focus == Model { "› " } else { "  " },
        if focus == Model {
            Color::Magenta
        } else {
            Color::DarkGrey
        },
        format!("Model     {}", model_modal.selected_model()),
        if focus == Model {
            Color::White
        } else {
            Color::Grey
        },
    ));
    lines.push(StyledLine::with_marker(
        if focus == Effort { "› " } else { "  " },
        if focus == Effort {
            Color::Magenta
        } else {
            Color::DarkGrey
        },
        format!("Effort    {}", model_modal.effort().label()),
        if focus == Effort {
            Color::White
        } else {
            Color::Grey
        },
    ));
    lines.push(StyledLine::with_marker(
        if focus == ApiKey { "› " } else { "  " },
        if focus == ApiKey {
            Color::Magenta
        } else {
            Color::DarkGrey
        },
        format!("API key   {}", model_modal.api_key_mask()),
        if focus == ApiKey {
            Color::White
        } else {
            Color::Grey
        },
    ));
    if focus == ApiKey {
        lines.push(StyledLine::new(
            "Type or paste a replacement key (used only for this Medusa session).",
            Color::DarkGrey,
        ));
    }
    lines.push(StyledLine::with_marker(
        if focus == Apply { "› " } else { "  " },
        if focus == Apply {
            Color::Magenta
        } else {
            Color::DarkGrey
        },
        "Apply configuration",
        if focus == Apply {
            Color::Green
        } else {
            Color::Grey
        },
    ));
    lines
}

pub(super) fn question_modal_lines(question_modal: &app::QuestionModal) -> Vec<StyledLine> {
    if question_modal.is_reviewing() {
        let mut lines = vec![StyledLine::new("Review answers", Color::Cyan)];
        for (index, prompt) in question_modal.questions().iter().enumerate() {
            lines.push(StyledLine::with_marker(
                "  ",
                Color::DarkGrey,
                format!(
                    "{}: {}",
                    prompt.header,
                    question_modal
                        .answer_for(index)
                        .unwrap_or_else(|| "Not answered".to_owned())
                ),
                if question_modal.answer_for(index).is_some() {
                    Color::White
                } else {
                    Color::Red
                },
            ));
        }
        lines.push(StyledLine::new(
            "Enter confirms and sends these answers",
            Color::Grey,
        ));
        return lines;
    }

    let Some(prompt) = question_modal.active_prompt() else {
        return vec![StyledLine::new("Question unavailable", Color::Red)];
    };
    let active = question_modal.active_question();
    let mut lines = vec![StyledLine::new(
        format!(
            "Questions {}/{}  [{}]",
            active.saturating_add(1),
            question_modal.questions().len(),
            question_modal
                .questions()
                .iter()
                .enumerate()
                .map(|(index, question)| {
                    if index == active {
                        format!("{}*", question.header)
                    } else {
                        question.header.clone()
                    }
                })
                .collect::<Vec<_>>()
                .join(" | ")
        ),
        Color::Cyan,
    )];
    lines.extend(
        prompt
            .question
            .lines()
            .map(|line| StyledLine::new(line.trim(), Color::White)),
    );
    for (index, option) in prompt.options.iter().enumerate() {
        let selected = index == question_modal.active_selected_option();
        lines.push(StyledLine::with_marker(
            if selected { "> " } else { "  " },
            if selected {
                Color::Magenta
            } else {
                Color::DarkGrey
            },
            if option.description.is_empty() {
                option.label.clone()
            } else {
                format!("{}  {}", option.label, option.description)
            },
            if selected { Color::White } else { Color::Grey },
        ));
    }
    let answer = question_modal.active_custom_answer();
    lines.push(StyledLine::with_marker(
        "> ",
        Color::Cyan,
        if answer.is_empty() {
            "Type a custom answer...".to_owned()
        } else {
            answer.to_owned()
        },
        if answer.is_empty() {
            Color::DarkGrey
        } else {
            Color::White
        },
    ));
    lines
}

pub(super) fn composer_prompt_text(text: &str) -> String {
    for prefix in ["/model key ", "/model api-key "] {
        if let Some(secret) = text.strip_prefix(prefix) {
            return format!("{prefix}{}", "*".repeat(secret.chars().count()));
        }
    }
    text.replace('\n', " / ")
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct StyledLine {
    marker: Option<(String, Color)>,
    pub(crate) text: String,
    foreground: Color,
}

impl StyledLine {
    pub(super) fn new(text: impl Into<String>, foreground: Color) -> Self {
        Self {
            marker: None,
            text: text.into(),
            foreground,
        }
    }

    pub(super) fn with_marker(
        marker: impl Into<String>,
        marker_color: Color,
        text: impl Into<String>,
        foreground: Color,
    ) -> Self {
        Self {
            marker: Some((marker.into(), marker_color)),
            text: text.into(),
            foreground,
        }
    }

    pub(super) fn print(&self, stdout: &mut io::Stdout, width: u16) -> io::Result<()> {
        if let Some((marker, marker_color)) = &self.marker {
            let marker = wrap_to_width(marker, width);
            let remaining = width.saturating_sub(marker.chars().count() as u16);
            return queue!(
                stdout,
                Clear(ClearType::UntilNewLine),
                SetAttribute(Attribute::Reset),
                ResetColor,
                SetForegroundColor(*marker_color),
                Print(marker),
                SetForegroundColor(self.foreground),
                Print(wrap_to_width(&self.text, remaining)),
                SetAttribute(Attribute::Reset),
                ResetColor,
                Print("\r\n")
            );
        }
        print_styled_line(stdout, width, &self.text, self.foreground, Attribute::Reset)
    }

    pub(super) fn print_at(&self, stdout: &mut io::Stdout, width: u16, row: u16) -> io::Result<()> {
        queue!(
            stdout,
            MoveTo(0, row),
            Clear(ClearType::CurrentLine),
            SetAttribute(Attribute::Reset),
            ResetColor,
        )?;
        if let Some((marker, marker_color)) = &self.marker {
            let marker = wrap_to_width(marker, width);
            let remaining = width.saturating_sub(marker.chars().count() as u16);
            queue!(
                stdout,
                SetForegroundColor(*marker_color),
                Print(marker),
                SetForegroundColor(self.foreground),
                Print(wrap_to_width(&self.text, remaining)),
            )?;
        } else {
            queue!(
                stdout,
                SetForegroundColor(self.foreground),
                Print(wrap_to_width(&self.text, width)),
            )?;
        }
        queue!(stdout, SetAttribute(Attribute::Reset), ResetColor)
    }
}

pub(super) fn system_line(message: &str) -> StyledLine {
    if message.starts_with("error:") {
        StyledLine::new(format!("● {message}"), Color::Red)
    } else if message.starts_with("evidence:") {
        StyledLine::new(format!("● {message}"), Color::Blue)
    } else if message.starts_with("step:") {
        StyledLine::new(format!("● {message}"), Color::Yellow)
    } else if message.contains("cancelled") {
        StyledLine::new(format!("● {message}"), Color::DarkYellow)
    } else {
        StyledLine::new(format!("● {message}"), Color::Green)
    }
}

pub(crate) fn activity_lines(activity: &TranscriptActivity) -> Vec<StyledLine> {
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
    if !matches!(
        activity.kind,
        TranscriptActivityKind::Assistant | TranscriptActivityKind::Tool
    ) {
        lines.extend(
            activity
                .details
                .iter()
                .map(|detail| StyledLine::new(format!("  └ {detail}"), Color::DarkGrey)),
        );
    }
    lines
}

pub(super) fn plan_lines(plan: &app::TranscriptPlan) -> Vec<StyledLine> {
    use app::TranscriptPlanStepState::{Active, Completed, Failed, Pending};

    plan.steps
        .iter()
        .map(|step| match step.state {
            Active => StyledLine::with_marker("▪ ", Color::Yellow, &step.title, Color::White),
            Completed => StyledLine::with_marker("✓ ", Color::Green, &step.title, Color::Grey),
            Failed => StyledLine::with_marker("✻ ", Color::Red, &step.title, Color::White),
            Pending => StyledLine::with_marker("□ ", Color::DarkGrey, &step.title, Color::DarkGrey),
        })
        .collect()
}

pub(super) fn print_separator(stdout: &mut io::Stdout, width: u16) -> io::Result<()> {
    queue!(
        stdout,
        Clear(ClearType::UntilNewLine),
        SetAttribute(Attribute::Reset),
        ResetColor,
        SetForegroundColor(Color::DarkGrey),
        Print("─".repeat(width as usize)),
        ResetColor,
        Print("\r\n")
    )
}

pub(super) fn print_styled_line(
    stdout: &mut io::Stdout,
    width: u16,
    text: &str,
    foreground: Color,
    attribute: Attribute,
) -> io::Result<()> {
    queue!(
        stdout,
        Clear(ClearType::UntilNewLine),
        SetAttribute(Attribute::Reset),
        ResetColor,
        SetForegroundColor(foreground),
        SetAttribute(attribute)
    )?;
    queue!(
        stdout,
        Print(wrap_to_width(text, width)),
        SetAttribute(Attribute::Reset),
        ResetColor,
        Print("\r\n")
    )
}

pub(crate) fn attachment_label(attachment: &PromptAttachment) -> String {
    match attachment {
        PromptAttachment::PastedText(text) => {
            format!("[text] {} | {} bytes", text.display_name, text.text.len())
        }
        PromptAttachment::Image(image) => format!(
            "[image] {} | {}x{} | {} bytes",
            image.display_name,
            image.width,
            image.height,
            image.rgba.len()
        ),
        PromptAttachment::File(file) => {
            format!("[file] {} | {} bytes", file.path.display(), file.byte_len)
        }
    }
}

/// Render `value` to a string that fits within `width` columns. Unlike
/// the previous `truncate`, this preserves the full content by wrapping
/// onto multiple lines joined with `\n`. A 0 width is treated as "no
/// limit" (the whole string is returned) so callers can pass through
/// when they don't know the terminal width yet.
pub fn wrap_to_width(value: &str, width: u16) -> String {
    let limit = usize::from(width);
    if limit == 0 || value.chars().count() <= limit {
        return value.to_owned();
    }
    let mut out = String::with_capacity(value.len() + value.len() / limit + 1);
    let mut col = 0usize;
    for ch in value.chars() {
        if ch == '\n' {
            out.push('\n');
            col = 0;
            continue;
        }
        if col >= limit {
            out.push('\n');
            col = 0;
        }
        out.push(ch);
        col += 1;
    }
    out
}

pub(crate) fn app_error(error: AppError) -> io::Error {
    io::Error::other(error)
}

pub(crate) fn runtime_error(error: runtime::RuntimeError) -> io::Error {
    io::Error::other(error)
}
