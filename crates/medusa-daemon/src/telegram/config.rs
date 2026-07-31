use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use super::{TelegramGatewayError, format::utf16_len};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TelegramTransport {
    Polling,
    Webhook,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolProgressMode {
    Off,
    New,
    All,
    Verbose,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TelegramVoiceMode {
    Off,
    VoiceOnly,
    All,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TelegramChatKind {
    #[default]
    Private,
    Group,
    Supergroup,
    Channel,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TelegramIdentity {
    pub user_id: i64,
    pub chat_id: i64,
    pub topic_id: Option<i64>,
    pub chat_kind: TelegramChatKind,
    pub bot_mentioned: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default)]
pub struct TelegramDisplayConfig {
    pub streaming: bool,
    pub edit_interval_ms: u64,
    pub buffer_threshold_chars: usize,
    pub cursor: String,
    pub fresh_final_after_seconds: u64,
    pub tool_progress: ToolProgressMode,
    pub interim_assistant_messages: bool,
    pub long_running_notifications: bool,
    pub busy_detail: bool,
    pub cleanup_progress: bool,
    pub disable_link_previews: bool,
}

impl Default for TelegramDisplayConfig {
    fn default() -> Self {
        Self {
            streaming: true,
            edit_interval_ms: 300,
            buffer_threshold_chars: 40,
            cursor: " ▉".to_owned(),
            fresh_final_after_seconds: 0,
            tool_progress: ToolProgressMode::New,
            interim_assistant_messages: true,
            long_running_notifications: true,
            busy_detail: false,
            cleanup_progress: false,
            disable_link_previews: false,
        }
    }
}

impl TelegramDisplayConfig {
    pub(crate) fn validate(&self) -> Result<(), TelegramGatewayError> {
        if !(100..=10_000).contains(&self.edit_interval_ms) {
            return Err(TelegramGatewayError::InvalidConfiguration(
                "edit interval must be between 100 and 10000 milliseconds".to_owned(),
            ));
        }
        if !(1..=4_000).contains(&self.buffer_threshold_chars) {
            return Err(TelegramGatewayError::InvalidConfiguration(
                "buffer threshold must be between 1 and 4000 characters".to_owned(),
            ));
        }
        if utf16_len(&self.cursor) > 16 {
            return Err(TelegramGatewayError::InvalidConfiguration(
                "streaming cursor is too long".to_owned(),
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default)]
pub struct TelegramVoiceConfig {
    pub mode: TelegramVoiceMode,
    pub transcription_model: String,
    pub voice: String,
    pub mini_app_enabled: bool,
    pub mini_app_public_url: Option<String>,
}

impl Default for TelegramVoiceConfig {
    fn default() -> Self {
        Self {
            mode: TelegramVoiceMode::Off,
            transcription_model: "gpt-4o-mini-transcribe".to_owned(),
            voice: "alloy".to_owned(),
            mini_app_enabled: false,
            mini_app_public_url: None,
        }
    }
}

impl TelegramVoiceConfig {
    fn validate(&self) -> Result<(), TelegramGatewayError> {
        if self.transcription_model.trim().is_empty() || self.voice.trim().is_empty() {
            return Err(TelegramGatewayError::InvalidConfiguration(
                "voice model and voice cannot be empty".to_owned(),
            ));
        }
        if self.mini_app_enabled {
            let Some(url) = self.mini_app_public_url.as_deref() else {
                return Err(TelegramGatewayError::InvalidConfiguration(
                    "Mini App requires a public HTTPS URL".to_owned(),
                ));
            };
            if !url.starts_with("https://") {
                return Err(TelegramGatewayError::InvalidConfiguration(
                    "Mini App URL must use HTTPS".to_owned(),
                ));
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default)]
pub struct TelegramConfig {
    pub enabled: bool,
    pub transport: TelegramTransport,
    pub repository_profile: String,
    pub allowed_users: BTreeSet<i64>,
    pub allowed_group_users: BTreeSet<i64>,
    pub allowed_chats: BTreeSet<i64>,
    pub require_mention: bool,
    pub home_chat_id: Option<i64>,
    pub home_topic_id: Option<i64>,
    pub webhook_secret_configured: bool,
    pub display: TelegramDisplayConfig,
    pub voice: TelegramVoiceConfig,
}

impl Default for TelegramConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            transport: TelegramTransport::Polling,
            repository_profile: "default".to_owned(),
            allowed_users: BTreeSet::new(),
            allowed_group_users: BTreeSet::new(),
            allowed_chats: BTreeSet::new(),
            require_mention: true,
            home_chat_id: None,
            home_topic_id: None,
            webhook_secret_configured: false,
            display: TelegramDisplayConfig::default(),
            voice: TelegramVoiceConfig::default(),
        }
    }
}

impl TelegramConfig {
    pub fn validate(&self) -> Result<(), TelegramGatewayError> {
        if !self.enabled {
            return Ok(());
        }
        if self.repository_profile.trim().is_empty() {
            return Err(TelegramGatewayError::InvalidConfiguration(
                "repository profile cannot be empty".to_owned(),
            ));
        }
        if self.allowed_users.is_empty() && self.allowed_group_users.is_empty() {
            return Err(TelegramGatewayError::InvalidConfiguration(
                "enabled gateway requires at least one numeric user allowlist entry".to_owned(),
            ));
        }
        if self.transport == TelegramTransport::Webhook && !self.webhook_secret_configured {
            return Err(TelegramGatewayError::InvalidConfiguration(
                "webhook mode requires a configured webhook secret".to_owned(),
            ));
        }
        if self.home_topic_id.is_some() && self.home_chat_id.is_none() {
            return Err(TelegramGatewayError::InvalidConfiguration(
                "a home topic requires a home chat".to_owned(),
            ));
        }
        self.display.validate()?;
        self.voice.validate()
    }

    pub fn authorize(&self, identity: &TelegramIdentity) -> Result<(), TelegramGatewayError> {
        if !self.enabled {
            return Err(TelegramGatewayError::Disabled);
        }
        match identity.chat_kind {
            TelegramChatKind::Private => {
                if !self.allowed_users.contains(&identity.user_id) {
                    return Err(TelegramGatewayError::Unauthorized);
                }
            }
            TelegramChatKind::Group | TelegramChatKind::Supergroup => {
                if !self.allowed_chats.contains(&identity.chat_id)
                    || !self.allowed_group_users.contains(&identity.user_id)
                {
                    return Err(TelegramGatewayError::Unauthorized);
                }
                if self.require_mention && !identity.bot_mentioned {
                    return Err(TelegramGatewayError::MentionRequired);
                }
            }
            TelegramChatKind::Channel => return Err(TelegramGatewayError::Unauthorized),
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enabled_config() -> TelegramConfig {
        TelegramConfig {
            enabled: true,
            allowed_users: BTreeSet::from([42]),
            allowed_group_users: BTreeSet::from([42]),
            allowed_chats: BTreeSet::from([-100]),
            ..TelegramConfig::default()
        }
    }

    fn private_identity() -> TelegramIdentity {
        TelegramIdentity {
            user_id: 42,
            chat_id: 42,
            topic_id: None,
            chat_kind: TelegramChatKind::Private,
            bot_mentioned: false,
        }
    }

    #[test]
    fn authorization_is_default_deny_and_numeric() {
        let config = enabled_config();
        config.authorize(&private_identity()).expect("allowed");
        let mut denied = private_identity();
        denied.user_id = 7;
        assert!(matches!(
            config.authorize(&denied),
            Err(TelegramGatewayError::Unauthorized)
        ));
    }

    #[test]
    fn group_authorization_requires_chat_user_and_mention() {
        let config = enabled_config();
        let mut identity = private_identity();
        identity.chat_id = -100;
        identity.chat_kind = TelegramChatKind::Supergroup;
        assert!(matches!(
            config.authorize(&identity),
            Err(TelegramGatewayError::MentionRequired)
        ));
        identity.bot_mentioned = true;
        config.authorize(&identity).expect("authorized group");
    }
}
