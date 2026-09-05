pub(super) mod markdown;
pub(super) mod support;

use super::*;
pub(crate) use support::*;
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

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
    total_tokens: u64,
    estimated_cost_microusd: u64,
    tokens_per_second_milli: u64,
    usage_provenance: Option<String>,
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
    activity_detail_expansion: Vec<bool>,
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
        total_tokens: app.total_tokens,
        estimated_cost_microusd: app.estimated_cost_microusd,
        tokens_per_second_milli: app.tokens_per_second_milli,
        usage_provenance: app.usage_provenance.clone(),
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
        activity_detail_expansion: app.activity_detail_expansion_snapshot(),
        spinner_frame: app.spinner_frame,
        selection: app.selection,
        model_modal: app.model_modal().cloned(),
        welcome_visible: app.welcome_visible(),
    }
}

fn active_status(app: &AppState) -> &str {
    app.transcript
        .iter()
        .rev()
        .find_map(|entry| match entry {
            TranscriptEntry::Activity(activity)
                if matches!(
                    activity.kind,
                    TranscriptActivityKind::Assistant
                        | TranscriptActivityKind::Progress
                        | TranscriptActivityKind::Tool
                        | TranscriptActivityKind::Verification
                ) =>
            {
                Some(activity.title.as_str())
            }
            _ => None,
        })
        .unwrap_or(&app.status)
}

pub(super) fn running_status(app: &AppState) -> String {
    format!(
        "working... · {}",
        format_elapsed(app.elapsed_seconds().unwrap_or_default())
    )
}

pub(super) fn session_metrics_line(app: &AppState, width: u16) -> String {
    let elapsed = format_elapsed(app.session_elapsed_seconds());
    let total = format_token_count(app.total_tokens);
    let cost = format_cost(app.estimated_cost_microusd);
    if width < 80 {
        return format!("session {elapsed} · total {total} · cost {cost}");
    }
    let rate = app
        .output_tokens_per_second()
        .map_or_else(|| "—".to_owned(), format_token_rate);
    if width < 120 {
        return format!(
            "session {elapsed} · total {total} · output {} · cost {cost} · {rate} tok/s",
            format_token_count(app.output_tokens),
        );
    }
    format!(
        "session {elapsed} · total {total} · input {} · output {} · cache-read {} · cache-write {} · cost {cost} · {} · {rate} tok/s",
        format_token_count(app.input_tokens),
        format_token_count(app.output_tokens),
        format_token_count(app.cache_read_input_tokens),
        format_token_count(app.cache_creation_input_tokens),
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
    let context = format!(
        "context [{bar}] {}/{} ({percent}%) · auto-compact {}%",
        format_token_count(used),
        format_token_count(window),
        app.auto_compact_percent(),
    );
    provider_plan_meter().map_or(context.clone(), |plan| format!("{context} · {plan}"))
}

#[derive(Clone, Debug, serde::Deserialize)]
struct ProviderPlanUsageSnapshot {
    provider: String,
    window_seconds: u64,
    used_basis_points: u16,
    reset_at_unix: Option<i64>,
    observed_at_unix: i64,
}

impl ProviderPlanUsageSnapshot {
    fn reset_after_seconds(&self, now_unix: i64) -> Option<u64> {
        self.reset_at_unix
            .and_then(|reset| u64::try_from(reset.saturating_sub(now_unix)).ok())
            .filter(|seconds| *seconds > 0)
    }
}

struct ProviderPlanUsageCacheEntry {
    observed_at: Instant,
    usage: Option<ProviderPlanUsageSnapshot>,
}

type ProviderPlanUsageCache = Mutex<Option<ProviderPlanUsageCacheEntry>>;

fn read_provider_plan_usage() -> Option<ProviderPlanUsageSnapshot> {
    let catalog = medusa_config::ProviderProfileCatalog::user().ok()?;
    let path = catalog.root().join("provider-plan-usage.json");
    let bytes = std::fs::read(path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn provider_plan_meter() -> Option<String> {
    static CACHE: OnceLock<ProviderPlanUsageCache> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(None));
    let mut cache = cache.lock().ok()?;
    let refresh = cache
        .as_ref()
        .is_none_or(|entry| entry.observed_at.elapsed() >= Duration::from_secs(1));
    if refresh {
        *cache = Some(ProviderPlanUsageCacheEntry {
            observed_at: Instant::now(),
            usage: read_provider_plan_usage(),
        });
    }
    let usage = cache.as_ref()?.usage.as_ref()?;
    let now = time::OffsetDateTime::now_utc().unix_timestamp();
    let window_seconds = i64::try_from(usage.window_seconds).unwrap_or(i64::MAX);
    let fresh_until = usage
        .reset_at_unix
        .unwrap_or_else(|| usage.observed_at_unix.saturating_add(window_seconds));
    if fresh_until <= now {
        return None;
    }
    const SEGMENTS: u16 = 10;
    let filled = usage.used_basis_points.saturating_mul(SEGMENTS) / 10_000;
    let bar = format!(
        "{}{}",
        "█".repeat(usize::from(filled)),
        "░".repeat(usize::from(SEGMENTS.saturating_sub(filled)))
    );
    let percent = u32::from(usage.used_basis_points) / 100;
    let window = format_window(usage.window_seconds);
    let reset = usage
        .reset_after_seconds(now)
        .map(|seconds| format!(" · resets {}", format_window(seconds)))
        .unwrap_or_default();
    Some(format!(
        "plan {} {window} [{bar}] {percent}%{reset}",
        usage.provider
    ))
}

fn format_window(seconds: u64) -> String {
    let hours = seconds / 3_600;
    let minutes = (seconds % 3_600) / 60;
    if hours > 0 && minutes > 0 {
        format!("{hours}h{minutes:02}m")
    } else if hours > 0 {
        format!("{hours}h")
    } else if minutes > 0 {
        format!("{minutes}m")
    } else {
        format!("{seconds}s")
    }
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
    project: String,
    model: String,
    effort: String,
    permission: String,
    build: Option<String>,
}

impl UiIdentity {
    pub(super) fn for_repo(repo: &Path) -> Self {
        Self::for_repo_with_build(repo, None)
    }

    pub(super) fn for_repo_with_build(repo: &Path, build: Option<&str>) -> Self {
        let project = repo.join(".medusa/config.toml");
        let project = project.exists().then_some(project);
        let config =
            Config::load_layers(None, project.as_deref(), &BTreeMap::new(), &BTreeMap::new())
                .unwrap_or_default();
        Self {
            project: repo
                .file_name()
                .and_then(|name| name.to_str())
                .filter(|name| !name.is_empty())
                .unwrap_or("General chat")
                .to_owned(),
            model: config.model.name,
            effort: effort_label(config.agent.max_turns).to_owned(),
            permission: PermissionStore::user()
                .and_then(|store| store.load())
                .map_or_else(
                    |_| PermissionMode::FullAccess.label().to_owned(),
                    |mode| mode.label().to_owned(),
                ),
            build: build.map(str::to_owned),
        }
    }

    pub(super) fn set_permission(&mut self, mode: PermissionMode) {
        self.permission = mode.label().to_owned();
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

fn modal_lines(model_modal: &app::ModelModal) -> Vec<StyledLine> {
    if model_modal.is_settings() {
        settings_modal_lines(model_modal)
    } else {
        support::model_modal_lines(model_modal)
    }
}

fn settings_modal_lines(modal: &app::ModelModal) -> Vec<StyledLine> {
    let page = modal.settings_page().unwrap_or(app::SettingsPage::Root);
    let revision = modal.settings_revision().unwrap_or_default();
    let profile = modal.settings_active_profile().unwrap_or("default");
    if page == app::SettingsPage::Root {
        let mut lines = vec![StyledLine::new(
            format!("Settings · profile {profile} · revision {revision}"),
            Color::Cyan,
        )];
        for (index, (label, value)) in modal.settings_root_rows().into_iter().enumerate() {
            let selected = index == modal.settings_root_selected();
            lines.push(StyledLine::with_marker(
                if selected { "› " } else { "  " },
                if selected {
                    Color::Magenta
                } else {
                    Color::DarkGrey
                },
                format!("{label:<15} {value}"),
                if selected { Color::White } else { Color::Grey },
            ));
        }
        lines.push(StyledLine::new(
            format!(
                "Last apply timing: {} · credentials remain external/redacted",
                modal
                    .settings_last_apply_timing()
                    .map_or("none", |timing| timing.label())
            ),
            Color::DarkGrey,
        ));
        return lines;
    }

    let page_name = match page {
        app::SettingsPage::Root => "Settings",
        app::SettingsPage::Profile => "Profile",
        app::SettingsPage::Provider => "Provider",
        app::SettingsPage::Model => "Model",
        app::SettingsPage::Speed => "Speed",
        app::SettingsPage::Reasoning => "Reasoning",
        app::SettingsPage::Authentication => "Authentication",
        app::SettingsPage::BaseUrl => "Base URL",
        app::SettingsPage::Status => "Status",
        app::SettingsPage::Review => "Review changes",
    };
    let mut lines = vec![StyledLine::new(
        format!("Settings / {page_name} · revision {revision}"),
        Color::Cyan,
    )];
    if page == app::SettingsPage::Status {
        lines.push(StyledLine::new(
            format!(
                "Health: {} · active profile: {profile}",
                modal.settings_doctor_summary()
            ),
            Color::White,
        ));
        let selected = modal.settings_selected_choice();
        for (index, check) in modal.settings_doctor_checks().into_iter().enumerate() {
            let is_selected = index == selected;
            lines.push(StyledLine::with_marker(
                if is_selected { "› " } else { "  " },
                if is_selected {
                    Color::Magenta
                } else {
                    Color::DarkGrey
                },
                format!(
                    "[{}] {} · {}",
                    check.status.label(),
                    check.name,
                    check.detail
                ),
                if is_selected {
                    Color::White
                } else {
                    Color::Grey
                },
            ));
            if is_selected {
                if let Some(remediation) = check.remediation {
                    lines.push(StyledLine::new(
                        format!("    {remediation}"),
                        Color::DarkGrey,
                    ));
                }
                if check.repair.is_some() {
                    lines.push(StyledLine::new(
                        "    Enter applies this deterministic repair through ProviderProfileCatalog.",
                        Color::DarkGrey,
                    ));
                } else {
                    lines.push(StyledLine::new(
                        "    Enter refreshes diagnostics; no automatic mutation is available for this check.",
                        Color::DarkGrey,
                    ));
                }
            }
        }
        lines.push(StyledLine::new(
            "Credentials remain external/redacted · Esc returns without applying a repair.",
            Color::DarkGrey,
        ));
        return lines;
    }
    if page == app::SettingsPage::Review {
        let review = modal.settings_review_lines();
        if review.is_empty() {
            lines.push(StyledLine::new("No staged changes.", Color::Grey));
        } else {
            lines.push(StyledLine::new("Pending non-secret changes:", Color::White));
            for change in review {
                lines.push(StyledLine::new(format!("  {change}"), Color::Grey));
            }
            lines.push(StyledLine::new(
                "Enter applies all staged changes atomically · Esc returns without applying.",
                Color::DarkGrey,
            ));
        }
        return lines;
    }
    if page == app::SettingsPage::BaseUrl {
        lines.push(StyledLine::with_marker(
            "> ",
            Color::Magenta,
            if modal.settings_base_url_edit().is_empty() {
                "provider default".to_owned()
            } else {
                modal.settings_base_url_edit().to_owned()
            },
            Color::White,
        ));
        lines.push(StyledLine::new(
            "Empty uses the provider default. Managed provider routes reject custom endpoints.",
            Color::DarkGrey,
        ));
        return lines;
    }
    if modal.settings_searching() || !modal.settings_search().is_empty() {
        lines.push(StyledLine::with_marker(
            "/ ",
            Color::Magenta,
            modal.settings_search(),
            Color::White,
        ));
    }
    let query = modal.settings_search().trim().to_lowercase();
    for (index, choice) in modal.settings_choices().into_iter().enumerate() {
        if !query.is_empty() && !choice.label.to_lowercase().contains(&query) {
            continue;
        }
        let selected = index == modal.settings_selected_choice();
        let description = if choice.description.is_empty() {
            choice.label.clone()
        } else {
            format!("{}  {}", choice.label, choice.description)
        };
        lines.push(StyledLine::with_marker(
            if selected { "› " } else { "  " },
            if selected {
                Color::Magenta
            } else {
                Color::DarkGrey
            },
            description,
            if !choice.enabled {
                Color::DarkGrey
            } else if selected {
                Color::White
            } else {
                Color::Grey
            },
        ));
    }
    lines
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
    let status = if app.is_waiting_for_answer() {
        running_status(app)
    } else {
        active_status(app).to_owned()
    };
    set_frame_line(
        &mut frame,
        row,
        StyledLine::new(
            format!(
                "{} · {} {}{}",
                identity.project,
                app.model_label.as_deref().unwrap_or(&identity.model),
                app.effort_label.as_deref().unwrap_or(&identity.effort),
                identity
                    .build
                    .as_deref()
                    .map(|build| format!(" · {build}"))
                    .unwrap_or_default(),
            ),
            Color::Cyan,
        ),
    );
    row = row.saturating_add(1);
    set_frame_line(&mut frame, row, StyledLine::new(status, Color::DarkGrey));

    let header_height = HEADER_TOP_PADDING + 5;
    let question_modal = app.question_modal();
    let model_modal = app.model_modal();
    let modal_lines = question_modal
        .map(question_modal_lines)
        .or_else(|| model_modal.map(modal_lines))
        .unwrap_or_default();
    let is_modal = question_modal.is_some() || model_modal.is_some();
    let plan_panel = if !is_modal && app.task_list_visible {
        app.plan.as_ref().map(plan_lines).unwrap_or_default()
    } else {
        Vec::new()
    };
    let panel_rows = u16::try_from(plan_panel.len()).unwrap_or(u16::MAX);
    let base_composer_rows = 6_u16.saturating_add(panel_rows);
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
    if app.is_waiting_for_answer() {
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
    if is_modal {
        set_frame_line(&mut frame, bottom_row, separator_line(width));
        bottom_row = bottom_row.saturating_add(1);
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
        } else if let Some(model_modal) = model_modal
            && model_modal.is_settings()
        {
            match model_modal
                .settings_page()
                .unwrap_or(app::SettingsPage::Root)
            {
                app::SettingsPage::Root => "up/down choose - enter open - esc close",
                app::SettingsPage::BaseUrl => "type endpoint - enter apply - esc back",
                app::SettingsPage::Status => "enter/esc back",
                _ if model_modal.settings_searching() => {
                    "type search - up/down choose - enter apply - esc clear search"
                }
                _ => "up/down choose - / search - enter apply - esc back",
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
    set_frame_line(
        &mut frame,
        bottom_row,
        StyledLine::new("─".repeat(usize::from(width)), Color::White),
    );
    bottom_row = bottom_row.saturating_add(1);
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
    set_frame_line(
        &mut frame,
        bottom_row,
        StyledLine::new("─".repeat(usize::from(width)), Color::White),
    );
    bottom_row = bottom_row.saturating_add(1);
    set_frame_line(
        &mut frame,
        bottom_row,
        StyledLine::with_marker(
            "> ",
            Color::Magenta,
            if app.is_running() {
                "shift+tab confirmation · enter queue follow-up - ctrl+c stop - ctrl+t session details · ctrl+e activity details"
            } else {
                "shift+tab confirmation · enter submit - ctrl+v paste - tab commands - ctrl+t session details · ctrl+e activity details"
            },
            Color::DarkGrey,
        ),
    );
    bottom_row = bottom_row.saturating_add(1);
    set_frame_line(
        &mut frame,
        bottom_row,
        StyledLine::new(
            format!(
                "{} · confirmation [{}]",
                session_metrics_line(app, width),
                identity.permission
            ),
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
