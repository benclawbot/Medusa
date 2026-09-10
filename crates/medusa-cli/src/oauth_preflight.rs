use std::env;

use medusa_config::{Config, openai_oauth};
use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};

const PREFLIGHT_ENV: &str = "MEDUSA_OAUTH_PREFLIGHT";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PreflightMode {
    Fast,
    Full,
    Off,
}

pub(crate) fn run_if_needed(config: &Config) -> MedusaResult<()> {
    if !eager_model_command() || !requires_preflight(config) {
        return Ok(());
    }
    let mode = preflight_mode();
    if mode == PreflightMode::Off {
        eprintln!(
            "ChatGPT OAuth app-server preflight is disabled by {PREFLIGHT_ENV}; login and model compatibility will be checked on the first turn."
        );
        return Ok(());
    }
    let models = medusa_runtime::ensure_openai_oauth_connected().map_err(|error| {
        preflight_error(
            ErrorCategory::Environment,
            format!("Codex app-server OAuth readiness failed: {error}"),
        )
    })?;
    verify_model(&models, &config.model.name)?;
    if mode == PreflightMode::Full {
        eprintln!(
            "ChatGPT OAuth app-server verified: model={} ({} models available; tool execution, streaming, and cancellation are exercised on live turns).",
            config.model.name,
            models.len()
        );
    } else {
        eprintln!(
            "ChatGPT OAuth app-server ready: model={} (fast preflight; live turn behavior is deferred; set {PREFLIGHT_ENV}=full for a stricter startup check).",
            config.model.name
        );
    }
    Ok(())
}

fn requires_preflight(config: &Config) -> bool {
    config.model.provider == openai_oauth::PROVIDER
}

fn eager_model_command() -> bool {
    eager_model_command_for(
        env::args_os()
            .skip(1)
            .filter_map(|argument| argument.into_string().ok()),
    )
}

/// Headless `run`/`resume` always need a live OAuth session, and so does the
/// interactive terminal, which is `medusa` with no subcommand at all.
fn eager_model_command_for(args: impl IntoIterator<Item = String>) -> bool {
    const SUBCOMMANDS: &[&str] = &[
        "bootstrap",
        "doctor",
        "health",
        "migrate",
        "config",
        "update",
        "search",
        "shell",
        "checkpoint",
        "request-audit",
        "telegram",
        "__daemon-serve",
        "run",
        "resume",
    ];
    let args: Vec<String> = args.into_iter().collect();
    if args
        .iter()
        .any(|argument| argument == "run" || argument == "resume")
    {
        return true;
    }
    !args
        .iter()
        .any(|argument| SUBCOMMANDS.contains(&argument.as_str()))
}

fn preflight_mode() -> PreflightMode {
    preflight_mode_for(env::var(PREFLIGHT_ENV).ok().as_deref())
}

fn preflight_mode_for(value: Option<&str>) -> PreflightMode {
    match value.map(|value| value.trim().to_ascii_lowercase()) {
        Some(value) if matches!(value.as_str(), "0" | "false" | "off" | "skip" | "disabled") => {
            PreflightMode::Off
        }
        Some(value) if matches!(value.as_str(), "full" | "strict") => PreflightMode::Full,
        _ => PreflightMode::Fast,
    }
}

fn verify_model(models: &[String], model: &str) -> MedusaResult<()> {
    if models.iter().any(|available| available == model) {
        Ok(())
    } else {
        Err(preflight_error(
            ErrorCategory::Validation,
            format!("configured OAuth model `{model}` is not exposed by Codex app-server"),
        ))
    }
}

fn preflight_error(category: ErrorCategory, message: impl Into<String>) -> MedusaError {
    MedusaError::new(ErrorCode::DependencyUnavailable, category, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_oauth_provider_requires_preflight() {
        let mut config = Config::default();
        assert!(!requires_preflight(&config));

        config.model.provider = openai_oauth::PROVIDER.into();
        assert!(requires_preflight(&config));

        config.model.provider = "openai".into();
        assert!(!requires_preflight(&config));
    }

    #[test]
    fn preflight_defaults_to_fast_and_allows_explicit_full_probe() {
        assert_eq!(preflight_mode_for(None), PreflightMode::Fast);
        assert_eq!(preflight_mode_for(Some("full")), PreflightMode::Full);
        assert_eq!(preflight_mode_for(Some("strict")), PreflightMode::Full);
        assert_eq!(preflight_mode_for(Some("off")), PreflightMode::Off);
    }

    #[test]
    fn model_discovery_requires_requested_model() {
        verify_model(&["gpt-test".to_owned()], "gpt-test").expect("model");
        assert!(verify_model(&["other".to_owned()], "gpt-test").is_err());
    }

    #[test]
    fn interactive_terminal_without_subcommand_runs_preflight() {
        assert!(eager_model_command_for(Vec::<String>::new()));
        assert!(eager_model_command_for(["--repo".into(), ".".into()]));
        assert!(eager_model_command_for(["run".into(), "do it".into()]));
        assert!(eager_model_command_for(["resume".into(), "abc".into()]));
        assert!(!eager_model_command_for(["doctor".into()]));
        assert!(!eager_model_command_for([
            "--repo".into(),
            ".".into(),
            "update".into()
        ]));
    }

    #[test]
    fn dependency_failures_name_the_app_server() {
        let error = preflight_error(
            ErrorCategory::Environment,
            "Codex app-server OAuth readiness failed",
        );
        assert!(error.message.contains("app-server"));
    }
}
