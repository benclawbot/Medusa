use std::{
    collections::BTreeSet,
    env, fs,
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use clap::{Args, ValueEnum};
use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use medusa_daemon::{
    DaemonLaunch, DaemonSupervisor,
    telegram::{
        OpenAiAudioToken,
        bot_api::{TelegramBotApiClient, TelegramBotToken},
        TelegramConfig, TelegramControl, TelegramDisplayConfig, TelegramGateway,
        TelegramPollingConfig, TelegramPollingRuntime, TelegramServiceMode,
        TelegramServiceSupervisor, TelegramSessionService, TelegramTransport,
        TelegramVoiceConfig, TelegramVoiceMode, TelegramVoicePipeline, TelegramWebhookConfig,
        TelegramWebhookServer, ToolProgressMode,
    },
};

const DEFAULT_TOKEN_ENV: &str = "MEDUSA_TELEGRAM_BOT_TOKEN";
const DEFAULT_WEBHOOK_SECRET_ENV: &str = "MEDUSA_TELEGRAM_WEBHOOK_SECRET";
const DEFAULT_OPENAI_TOKEN_ENV: &str = "OPENAI_API_KEY";

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum TransportArg {
    Polling,
    Webhook,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum ProgressArg {
    Off,
    New,
    All,
    Verbose,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum VoiceArg {
    Off,
    VoiceOnly,
    All,
}

#[derive(Args, Clone, Debug)]
pub struct TelegramArgs {
    /// Environment variable containing the Telegram Bot API token.
    #[arg(long, default_value = DEFAULT_TOKEN_ENV)]
    token_env: String,
    /// Bot username; when omitted it is resolved with Telegram getMe.
    #[arg(long)]
    bot_username: Option<String>,
    /// Numeric Telegram users allowed in private chats. Repeat for multiple users.
    #[arg(long = "allow-user", required = true)]
    allowed_users: Vec<i64>,
    /// Numeric Telegram users allowed to control the bot in groups.
    #[arg(long = "allow-group-user")]
    allowed_group_users: Vec<i64>,
    /// Numeric Telegram group/supergroup chats allowed to use the bot.
    #[arg(long = "allow-chat", allow_hyphen_values = true)]
    allowed_chats: Vec<i64>,
    /// Permit group messages without an explicit bot mention.
    #[arg(long)]
    no_require_mention: bool,
    /// Repository profile sent when Telegram creates a new session.
    #[arg(long, default_value = "default")]
    repository_profile: String,
    #[arg(long, value_enum, default_value_t = TransportArg::Polling)]
    transport: TransportArg,
    #[arg(long, default_value_t = 30)]
    poll_timeout_seconds: u16,
    #[arg(long, default_value_t = 100)]
    poll_limit: u8,
    /// Process one polling cycle and exit. Intended for deployment conformance checks.
    #[arg(long)]
    once: bool,
    /// Repository-scoped durable Telegram transport state.
    #[arg(long, default_value = ".medusa/telegram/state.json")]
    state_path: PathBuf,
    #[arg(long, value_enum, default_value_t = ProgressArg::New)]
    tool_progress: ProgressArg,
    #[arg(long, value_enum, default_value_t = VoiceArg::Off)]
    voice: VoiceArg,
    #[arg(long, default_value = DEFAULT_OPENAI_TOKEN_ENV)]
    openai_token_env: String,
    #[arg(long, default_value = "https://api.openai.com")]
    openai_api_base: String,
    #[arg(long, default_value = "gpt-4o-mini-transcribe")]
    transcription_model: String,
    #[arg(long, default_value = "gpt-4o-mini-tts")]
    tts_model: String,
    #[arg(long, default_value = "alloy")]
    tts_voice: String,
    #[arg(long)]
    home_chat_id: Option<i64>,
    #[arg(long)]
    home_topic_id: Option<i64>,
    /// Public HTTPS Telegram webhook URL, including the configured path.
    #[arg(long)]
    webhook_public_url: Option<String>,
    #[arg(long, default_value = "127.0.0.1:8787")]
    webhook_bind: SocketAddr,
    #[arg(long, default_value = "/telegram/update")]
    webhook_path: String,
    #[arg(long, default_value = DEFAULT_WEBHOOK_SECRET_ENV)]
    webhook_secret_env: String,
    #[arg(long)]
    drop_pending_updates: bool,
}

pub fn run(repo: &Path, args: TelegramArgs) -> MedusaResult<()> {
    fs::create_dir_all(repo.join(".medusa/telegram"))?;
    env::set_current_dir(repo)?;

    let token = TelegramBotToken::new(read_secret(&args.token_env)?)
        .map_err(telegram_error)?;
    let client = TelegramBotApiClient::new(token).map_err(telegram_error)?;
    let bot_username = match args.bot_username.as_deref() {
        Some(value) => value.trim().trim_start_matches('@').to_owned(),
        None => client
            .get_me()
            .map_err(telegram_error)?
            .username
            .ok_or_else(|| invalid("Telegram bot account has no username"))?,
    };

    let mut telegram_config = gateway_config(&args);
    let webhook_secret = match args.transport {
        TransportArg::Polling => None,
        TransportArg::Webhook => {
            telegram_config.webhook_secret_configured = true;
            Some(read_secret(&args.webhook_secret_env)?)
        }
    };
    telegram_config.validate().map_err(telegram_error)?;
    let gateway = TelegramGateway::new(telegram_config.clone()).map_err(telegram_error)?;

    let launch = DaemonLaunch::for_current_executable()?;
    let mut daemon = DaemonSupervisor::new(repo, launch);
    daemon.ensure_running()?;
    let control = TelegramControl::from(daemon.client());
    let state_path = resolve_path(repo, &args.state_path);
    let service = TelegramSessionService::load(state_path, gateway, control)
        .map_err(telegram_error)?;
    let mut runtime = TelegramPollingRuntime::new(
        client.clone(),
        service,
        TelegramPollingConfig {
            bot_username,
            timeout_seconds: args.poll_timeout_seconds,
            limit: args.poll_limit,
        },
    )
    .map_err(telegram_error)?;

    if telegram_config.voice.mode != TelegramVoiceMode::Off {
        let pipeline = TelegramVoicePipeline::new(
            OpenAiAudioToken::new(read_secret(&args.openai_token_env)?)
                .map_err(telegram_error)?,
            args.openai_api_base,
            args.transcription_model,
            args.tts_model,
            args.tts_voice,
        )
        .map_err(telegram_error)?;
        runtime = runtime.with_voice_pipeline(pipeline);
    }

    if args.once {
        if args.transport != TransportArg::Polling {
            return Err(invalid("--once is supported only with polling transport"));
        }
        runtime.poll_once().map_err(telegram_error)?;
        return Ok(());
    }

    let mode = match args.transport {
        TransportArg::Polling => TelegramServiceMode::Polling,
        TransportArg::Webhook => {
            let public_url = args
                .webhook_public_url
                .filter(|value| value.starts_with("https://"))
                .ok_or_else(|| invalid("webhook transport requires --webhook-public-url https://..."))?;
            let secret_token = webhook_secret.expect("validated webhook secret");
            let server = TelegramWebhookServer::bind(TelegramWebhookConfig {
                bind: args.webhook_bind,
                path: args.webhook_path,
                secret_token: secret_token.clone(),
            })
            .map_err(telegram_error)?;
            TelegramServiceMode::Webhook {
                server,
                public_url,
                secret_token,
                drop_pending_updates: args.drop_pending_updates,
            }
        }
    };

    let cancelled = Arc::new(AtomicBool::new(false));
    let signal = Arc::clone(&cancelled);
    ctrlc::set_handler(move || signal.store(true, Ordering::Release))
        .map_err(|error| environment(format!("install Telegram shutdown handler: {error}")))?;
    TelegramServiceSupervisor::new(client, runtime, mode)
        .run(cancelled)
        .map_err(telegram_error)
}

fn gateway_config(args: &TelegramArgs) -> TelegramConfig {
    TelegramConfig {
        enabled: true,
        transport: match args.transport {
            TransportArg::Polling => TelegramTransport::Polling,
            TransportArg::Webhook => TelegramTransport::Webhook,
        },
        repository_profile: args.repository_profile.clone(),
        allowed_users: args.allowed_users.iter().copied().collect::<BTreeSet<_>>(),
        allowed_group_users: args
            .allowed_group_users
            .iter()
            .copied()
            .collect::<BTreeSet<_>>(),
        allowed_chats: args.allowed_chats.iter().copied().collect::<BTreeSet<_>>(),
        require_mention: !args.no_require_mention,
        home_chat_id: args.home_chat_id,
        home_topic_id: args.home_topic_id,
        webhook_secret_configured: args.transport == TransportArg::Webhook,
        display: TelegramDisplayConfig {
            tool_progress: match args.tool_progress {
                ProgressArg::Off => ToolProgressMode::Off,
                ProgressArg::New => ToolProgressMode::New,
                ProgressArg::All => ToolProgressMode::All,
                ProgressArg::Verbose => ToolProgressMode::Verbose,
            },
            ..TelegramDisplayConfig::default()
        },
        voice: TelegramVoiceConfig {
            mode: match args.voice {
                VoiceArg::Off => TelegramVoiceMode::Off,
                VoiceArg::VoiceOnly => TelegramVoiceMode::VoiceOnly,
                VoiceArg::All => TelegramVoiceMode::All,
            },
            transcription_model: args.transcription_model.clone(),
            voice: args.tts_voice.clone(),
            ..TelegramVoiceConfig::default()
        },
    }
}

fn read_secret(name: &str) -> MedusaResult<String> {
    if name.is_empty()
        || name.len() > 128
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
    {
        return Err(invalid("secret environment variable name is invalid"));
    }
    let value = env::var(name)
        .map_err(|_| environment(format!("required secret environment variable {name} is not set")))?;
    if value.trim().is_empty() {
        return Err(environment(format!(
            "required secret environment variable {name} is empty"
        )));
    }
    Ok(value)
}

fn resolve_path(repo: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        repo.join(path)
    }
}

fn telegram_error(error: impl std::fmt::Display) -> MedusaError {
    MedusaError::new(
        ErrorCode::DependencyUnavailable,
        ErrorCategory::Environment,
        error.to_string(),
    )
}

fn invalid(message: impl Into<String>) -> MedusaError {
    MedusaError::new(
        ErrorCode::InvalidConfiguration,
        ErrorCategory::Validation,
        message,
    )
}

fn environment(message: impl Into<String>) -> MedusaError {
    MedusaError::new(
        ErrorCode::DependencyUnavailable,
        ErrorCategory::Environment,
        message,
    )
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;

    #[derive(Parser)]
    struct Harness {
        #[command(flatten)]
        telegram: TelegramArgs,
    }

    #[test]
    fn command_requires_a_numeric_private_user_allowlist() {
        assert!(Harness::try_parse_from(["telegram"]).is_err());
        let parsed = Harness::try_parse_from([
            "telegram",
            "--allow-user",
            "42",
            "--allow-chat",
            "-100",
            "--allow-group-user",
            "42",
        ])
        .expect("parse Telegram command");
        let config = gateway_config(&parsed.telegram);
        assert!(config.allowed_users.contains(&42));
        assert!(config.allowed_chats.contains(&-100));
        assert!(config.require_mention);
    }

    #[test]
    fn webhook_mode_requires_secret_and_public_url_at_runtime() {
        let parsed = Harness::try_parse_from([
            "telegram",
            "--allow-user",
            "42",
            "--transport",
            "webhook",
            "--webhook-public-url",
            "https://example.test/telegram/update",
        ])
        .expect("parse webhook command");
        let config = gateway_config(&parsed.telegram);
        assert_eq!(config.transport, TelegramTransport::Webhook);
        assert!(config.webhook_secret_configured);
    }

    #[test]
    fn secret_names_are_bounded_and_uppercase() {
        assert!(read_secret("bad-name").is_err());
    }
}
