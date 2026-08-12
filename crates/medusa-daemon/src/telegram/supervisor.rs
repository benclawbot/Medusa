//! Operational supervisor for mutually exclusive Telegram polling and webhook transports.

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{TrySendError, sync_channel},
    },
    thread,
};

use super::{
    TelegramPollingRuntime, TelegramRuntimeError, TelegramWebhookServer,
    bot_api::{TelegramBotApiClient, TelegramBotCommand},
};

const WEBHOOK_UPDATE_QUEUE_CAPACITY: usize = 256;

pub enum TelegramServiceMode {
    Polling,
    Webhook {
        server: TelegramWebhookServer,
        public_url: String,
        secret_token: String,
        drop_pending_updates: bool,
    },
}

pub struct TelegramServiceSupervisor {
    client: TelegramBotApiClient,
    runtime: TelegramPollingRuntime,
    mode: TelegramServiceMode,
}

impl TelegramServiceSupervisor {
    #[must_use]
    pub fn new(
        client: TelegramBotApiClient,
        runtime: TelegramPollingRuntime,
        mode: TelegramServiceMode,
    ) -> Self {
        Self {
            client,
            runtime,
            mode,
        }
    }

    pub fn run(mut self, cancelled: Arc<AtomicBool>) -> Result<(), TelegramSupervisorError> {
        self.client.set_commands(&default_commands())?;
        match self.mode {
            TelegramServiceMode::Polling => {
                self.client.delete_webhook(false)?;
                self.runtime.run_until_cancelled(cancelled.as_ref())?;
                Ok(())
            }
            TelegramServiceMode::Webhook {
                server,
                public_url,
                secret_token,
                drop_pending_updates,
            } => {
                self.client
                    .set_webhook(&public_url, &secret_token, drop_pending_updates)?;
                let (sender, receiver) = sync_channel(WEBHOOK_UPDATE_QUEUE_CAPACITY);
                self.runtime = self.runtime.with_webhook_updates(receiver);
                let server_cancelled = Arc::clone(&cancelled);
                let handle = thread::Builder::new()
                    .name("medusa-telegram-webhook".to_owned())
                    .spawn(move || {
                        server.run_until_cancelled(server_cancelled.as_ref(), |update| {
                            sender.try_send(update).map_err(|error| match error {
                                TrySendError::Full(_) => "Telegram webhook update queue is full",
                                TrySendError::Disconnected(_) => {
                                    "Telegram webhook runtime is unavailable"
                                }
                            })
                        })
                    })
                    .map_err(TelegramSupervisorError::Thread)?;
                let runtime_result = self.runtime.run_until_cancelled(cancelled.as_ref());
                cancelled.store(true, Ordering::Release);
                let server_result = handle
                    .join()
                    .map_err(|_| TelegramSupervisorError::ThreadPanicked)?;
                let delete_result = self.client.delete_webhook(false);
                runtime_result?;
                server_result?;
                delete_result?;
                Ok(())
            }
        }
    }
}

fn default_commands() -> Vec<TelegramBotCommand> {
    [
        ("sessions", "List recent and active Medusa sessions"),
        ("new", "Create a new Medusa session"),
        ("attach", "Attach this chat to an existing session"),
        ("detach", "Detach without cancelling the session"),
        ("resume", "Resume an interrupted session"),
        ("status", "Show current session and runtime status"),
        ("stop", "Cancel the active Medusa turn"),
        ("toolprogress", "Configure Telegram action progress"),
        ("voice", "Configure voice-note and Mini App voice"),
        ("help", "Show Telegram gateway commands"),
    ]
    .into_iter()
    .map(|(command, description)| TelegramBotCommand {
        command: command.to_owned(),
        description: description.to_owned(),
    })
    .collect()
}

#[derive(Debug, thiserror::Error)]
pub enum TelegramSupervisorError {
    #[error(transparent)]
    BotApi(#[from] super::bot_api::TelegramBotApiError),
    #[error(transparent)]
    Runtime(#[from] TelegramRuntimeError),
    #[error(transparent)]
    Webhook(#[from] super::TelegramWebhookError),
    #[error("failed to start Telegram webhook thread: {0}")]
    Thread(std::io::Error),
    #[error("Telegram webhook thread panicked")]
    ThreadPanicked,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_menu_covers_session_control_and_voice() {
        let commands = default_commands();
        for required in ["sessions", "attach", "status", "stop", "voice"] {
            assert!(commands.iter().any(|command| command.command == required));
        }
    }
}
