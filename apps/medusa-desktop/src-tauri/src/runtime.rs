use std::{
    collections::{BTreeMap, VecDeque},
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use image::ImageReader;
use medusa_config::{Config, provider_catalog_entry};
use medusa_daemon::{
    DaemonClient, DaemonLaunch, DaemonLifecycleState, DaemonSupervisor, FrontendArtifactKind,
    FrontendArtifactUpload, FrontendCommandAcknowledgement, FrontendControlResult,
    FrontendCredentialUpdate, FrontendTransientEvent,
};
use medusa_protocol::frontend::{
    FRONTEND_PROTOCOL_VERSION, FrontendCommand, FrontendCommandEnvelope, FrontendKind,
};
use medusa_provider::discover_models;
use medusa_runtime::{
    attachment::{
        MAX_CLIPBOARD_TEXT_BYTES, MAX_IMAGE_BYTES, MAX_IMAGES_PER_PROMPT,
        MAX_TOTAL_ATTACHMENT_BYTES,
    },
    commands::command_suggestions,
    prompt::MAX_IMAGE_PIXELS,
};
use tauri::{AppHandle, Manager, State};
use time::OffsetDateTime;
use ulid::Ulid;

use crate::{
    config::{DesktopConfigurationChanged, prepare_provider_profile},
    credentials::{CredentialStore, SystemCredentialStore},
    dto::{
        DesktopAttachment, DesktopCommandSuggestion, DesktopModelConfiguration, DesktopPromptDraft,
        DesktopRuntimeEvent, DesktopSubmitDisposition, RuntimeStartResponse,
    },
    provider_auth::browser_oauth_credentials_present,
};

const DESKTOP_CLIENT_ID: &str = "desktop-primary";

struct DesktopDaemon {
    supervisor: DaemonSupervisor,
    last_state: Option<DaemonLifecycleState>,
}

struct RuntimeEntry {
    repo: PathBuf,
    client_id: String,
    session_id: Option<String>,
    replay_cursor: u64,
    pending_ack_cursor: Option<u64>,
    presentation: DesktopCanonicalPresentation,
    daemon: DesktopDaemon,
}

impl RuntimeEntry {
    fn daemon_event(&mut self) -> Option<DesktopRuntimeEvent> {
        let lifecycle = self.daemon.supervisor.poll();
        let suppress_connected_after_start = matches!(
            (self.daemon.last_state, lifecycle.state),
            (
                Some(DaemonLifecycleState::Started | DaemonLifecycleState::Recovered),
                DaemonLifecycleState::Connected
            )
        );
        let changed = self.daemon.last_state != Some(lifecycle.state);
        self.daemon.last_state = Some(lifecycle.state);
        if !changed || suppress_connected_after_start {
            return None;
        }
        Some(DesktopRuntimeEvent::Notice {
            title: format!("Background daemon {}", lifecycle.state.as_str()),
            details: vec![lifecycle.detail],
        })
    }

    fn ensure_daemon(&mut self) -> Result<(), String> {
        let lifecycle = self
            .daemon
            .supervisor
            .ensure_running()
            .map_err(|error| error.to_string())?;
        self.daemon.last_state = Some(lifecycle.state);
        Ok(())
    }

    fn client(&self) -> DaemonClient {
        self.daemon.supervisor.client()
    }

    fn envelope(&self, command: FrontendCommand) -> FrontendCommandEnvelope {
        let id = format!("desktop-command-{}", Ulid::new());
        FrontendCommandEnvelope {
            protocol_version: FRONTEND_PROTOCOL_VERSION,
            command_id: id.clone(),
            idempotency_key: id,
            frontend: FrontendKind::Desktop,
            client_id: self.client_id.clone(),
            session_id: self.session_id.clone(),
            turn_id: None,
            timestamp: OffsetDateTime::now_utc(),
            command,
        }
    }

    fn envelope_for_session(
        &self,
        session_id: String,
        command: FrontendCommand,
    ) -> FrontendCommandEnvelope {
        let mut envelope = self.envelope(command);
        envelope.session_id = Some(session_id);
        envelope
    }

    fn dispatch(
        &mut self,
        command: FrontendCommand,
    ) -> Result<FrontendCommandAcknowledgement, String> {
        self.ensure_daemon()?;
        self.client()
            .frontend(self.envelope(command))
            .map_err(|error| error.to_string())
    }

    fn dispatch_for_session(
        &mut self,
        session_id: String,
        command: FrontendCommand,
    ) -> Result<FrontendCommandAcknowledgement, String> {
        self.ensure_daemon()?;
        self.client()
            .frontend(self.envelope_for_session(session_id, command))
            .map_err(|error| error.to_string())
    }

    fn bind_attachment(&mut self, attachment: medusa_daemon::LiveSessionAttachmentView) {
        self.session_id = Some(attachment.session.id);
        self.replay_cursor = attachment.replay_cursor;
        if self.replay_cursor > attachment.acknowledged_cursor {
            self.pending_ack_cursor = Some(self.replay_cursor);
        }
        self.presentation.push(attachment.replay);
    }

    fn resume(&mut self, session_id: String) -> Result<(), String> {
        let acknowledgement = self.dispatch_for_session(
            session_id.clone(),
            FrontendCommand::ResumeSession {
                session_id: session_id.clone(),
            },
        )?;
        let FrontendControlResult::RuntimeReady { attachment } = acknowledgement.result else {
            return Err("daemon returned an unexpected resume result".to_owned());
        };
        self.bind_attachment(attachment);
        Ok(())
    }

    fn sync_credential(&mut self, provider: &str, credential: Option<String>) -> Result<(), String> {
        let Some(credential) = credential.filter(|value| !value.trim().is_empty()) else {
            return Ok(());
        };
        self.ensure_daemon()?;
        self.client()
            .frontend_credential(FrontendCredentialUpdate {
                provider: provider.to_owned(),
                credential,
            })
            .map_err(|error| error.to_string())
    }

    fn stage_draft(&mut self, draft: DesktopPromptDraft) -> Result<(String, Vec<String>), String> {
        let DesktopPromptDraft {
            text,
            attachments,
            revision,
        } = draft;
        let _advisory_revision = revision;
        let mut total = 0_usize;
        let mut images = 0_usize;
        let mut ids = Vec::with_capacity(attachments.len());
        for attachment in attachments {
            let upload = match attachment {
                DesktopAttachment::File { path } => {
                    let canonical = fs::canonicalize(&path)
                        .map_err(|error| format!("cannot attach {path}: {error}"))?;
                    if !canonical.starts_with(&self.repo) {
                        return Err(format!(
                            "attachment {} is outside the selected repository",
                            canonical.display()
                        ));
                    }
                    let bytes = fs::read(&canonical)
                        .map_err(|error| format!("cannot read {}: {error}", canonical.display()))?;
                    total = checked_total(total, bytes.len())?;
                    FrontendArtifactUpload {
                        display_name: canonical
                            .file_name()
                            .and_then(|value| value.to_str())
                            .unwrap_or("attachment.bin")
                            .to_owned(),
                        mime_type: None,
                        kind: FrontendArtifactKind::File,
                        bytes_base64: STANDARD.encode(bytes),
                    }
                }
                DesktopAttachment::Image { name, data_url } => {
                    images = images.saturating_add(1);
                    if images > MAX_IMAGES_PER_PROMPT {
                        return Err(format!(
                            "prompt allows at most {MAX_IMAGES_PER_PROMPT} images"
                        ));
                    }
                    let (header, encoded) = data_url
                        .split_once(',')
                        .ok_or_else(|| format!("image attachment {name} is not a data URL"))?;
                    if !header.starts_with("data:image/") || !header.ends_with(";base64") {
                        return Err(format!(
                            "image attachment {name} must be a base64 image data URL"
                        ));
                    }
                    let mime_type = header
                        .trim_start_matches("data:")
                        .trim_end_matches(";base64")
                        .to_ascii_lowercase();
                    if !matches!(
                        mime_type.as_str(),
                        "image/gif" | "image/jpeg" | "image/png" | "image/webp"
                    ) {
                        return Err(format!("image attachment {name} has unsupported type {mime_type}"));
                    }
                    let bytes = STANDARD
                        .decode(encoded)
                        .map_err(|error| format!("cannot decode image attachment {name}: {error}"))?;
                    if bytes.len() > MAX_IMAGE_BYTES {
                        return Err(format!(
                            "image attachment {name} is {} bytes; limit is {MAX_IMAGE_BYTES}",
                            bytes.len()
                        ));
                    }
                    let dimensions = ImageReader::new(std::io::Cursor::new(bytes.as_slice()))
                        .with_guessed_format()
                        .map_err(|error| format!("cannot detect image attachment {name}: {error}"))?
                        .into_dimensions()
                        .map_err(|error| format!("cannot inspect image attachment {name}: {error}"))?;
                    validate_image_dimensions(&name, dimensions.0, dimensions.1)?;
                    total = checked_total(total, bytes.len())?;
                    FrontendArtifactUpload {
                        display_name: name,
                        mime_type: Some(mime_type),
                        kind: FrontendArtifactKind::Image,
                        bytes_base64: STANDARD.encode(bytes),
                    }
                }
                DesktopAttachment::Text { name, text } => {
                    if text.len() > MAX_CLIPBOARD_TEXT_BYTES {
                        return Err(format!(
                            "text attachment {name} exceeds the clipboard text limit"
                        ));
                    }
                    total = checked_total(total, text.len())?;
                    FrontendArtifactUpload {
                        display_name: name,
                        mime_type: Some("text/plain".to_owned()),
                        kind: FrontendArtifactKind::Text,
                        bytes_base64: STANDARD.encode(text.as_bytes()),
                    }
                }
            };
            let id = self
                .client()
                .frontend_artifact(upload)
                .map_err(|error| error.to_string())?;
            ids.push(id);
        }
        Ok((text, ids))
    }

    fn acknowledge_previous_delivery(&mut self) -> Result<(), String> {
        let Some(cursor) = self.pending_ack_cursor.take() else {
            return Ok(());
        };
        let acknowledgement = self.dispatch(FrontendCommand::AcknowledgeCursor { cursor })?;
        if !matches!(
            acknowledgement.result,
            FrontendControlResult::CursorAcknowledged { .. }
        ) {
            return Err("daemon returned an unexpected cursor acknowledgement".to_owned());
        }
        Ok(())
    }

    fn poll_daemon(&mut self) -> Result<(), String> {
        let Some(session_id) = self.session_id.clone() else {
            return Ok(());
        };
        self.acknowledge_previous_delivery()?;

        let transient = self.dispatch(FrontendCommand::PollTransient)?;
        let FrontendControlResult::Transient { events } = transient.result else {
            return Err("daemon returned an unexpected transient-event result".to_owned());
        };
        for event in events {
            if matches!(event, FrontendTransientEvent::NewSession) {
                self.presentation.reset();
                self.session_id = None;
                self.replay_cursor = 0;
                self.pending_ack_cursor = None;
            }
            self.presentation.push_transient(event);
        }
        if self.session_id.is_none() {
            return Ok(());
        }

        let replay = self.dispatch(FrontendCommand::Replay {
            after_cursor: self.replay_cursor,
        })?;
        let FrontendControlResult::Events { replay } = replay.result else {
            return Err("daemon returned an unexpected replay result".to_owned());
        };
        if replay.session_id != session_id {
            return Err("daemon replay switched sessions unexpectedly".to_owned());
        }
        self.replay_cursor = replay.next_cursor;
        self.presentation.push(replay.events);
        if replay.next_cursor > replay.after_cursor {
            self.pending_ack_cursor = Some(replay.next_cursor);
        }
        Ok(())
    }
}

#[derive(Default)]
pub struct RuntimeRegistry {
    next_id: AtomicU64,
    entries: Mutex<BTreeMap<String, Arc<Mutex<RuntimeEntry>>>>,
}

impl RuntimeRegistry {
    fn insert(
        &self,
        repo: PathBuf,
        displayed_repo: String,
    ) -> Result<RuntimeStartResponse, String> {
        let id = format!(
            "desktop-runtime-{}",
            self.next_id.fetch_add(1, Ordering::Relaxed) + 1
        );
        let launch = DaemonLaunch::for_current_executable().map_err(|error| error.to_string())?;
        let mut supervisor = DaemonSupervisor::new(&repo, launch);
        let lifecycle = supervisor
            .ensure_running()
            .map_err(|error| error.to_string())?;
        let entry = Arc::new(Mutex::new(RuntimeEntry {
            repo,
            client_id: DESKTOP_CLIENT_ID.to_owned(),
            session_id: None,
            replay_cursor: 0,
            pending_ack_cursor: None,
            presentation: DesktopCanonicalPresentation::new(),
            daemon: DesktopDaemon {
                supervisor,
                last_state: Some(lifecycle.state),
            },
        }));
        self.entries
            .lock()
            .map_err(|_| "desktop runtime registry is poisoned".to_owned())?
            .insert(id.clone(), entry);
        Ok(RuntimeStartResponse {
            runtime_id: id,
            repo: displayed_repo,
        })
    }

    fn with_entry<T>(
        &self,
        runtime_id: &str,
        action: impl FnOnce(&mut RuntimeEntry) -> Result<T, String>,
    ) -> Result<T, String> {
        let entry = self
            .entries
            .lock()
            .map_err(|_| "desktop runtime registry is poisoned".to_owned())?
            .get(runtime_id)
            .cloned()
            .ok_or_else(|| format!("runtime {runtime_id} does not exist"))?;
        let mut entry = entry
            .lock()
            .map_err(|_| format!("runtime {runtime_id} is poisoned"))?;
        action(&mut entry)
    }
}

#[tauri::command]
pub fn runtime_start(
    repo: Option<String>,
    app: AppHandle,
    registry: State<'_, RuntimeRegistry>,
) -> Result<RuntimeStartResponse, String> {
    let (runtime_repo, displayed_repo) = match repo {
        Some(repo) => {
            let runtime_repo = canonical_directory(Path::new(&repo))?;
            let displayed_repo = runtime_repo.to_string_lossy().into_owned();
            (runtime_repo, displayed_repo)
        }
        None => {
            let runtime_repo = app
                .path()
                .app_local_data_dir()
                .map_err(|error| format!("cannot locate Medusa application data: {error}"))?
                .join("general-chat");
            fs::create_dir_all(&runtime_repo).map_err(|error| {
                format!(
                    "cannot create general chat workspace {}: {error}",
                    runtime_repo.display()
                )
            })?;
            (canonical_directory(&runtime_repo)?, String::new())
        }
    };
    registry.insert(runtime_repo, displayed_repo)
}

#[tauri::command]
pub fn runtime_close(
    runtime_id: String,
    registry: State<'_, RuntimeRegistry>,
) -> Result<(), String> {
    registry
        .entries
        .lock()
        .map_err(|_| "desktop runtime registry is poisoned".to_owned())?
        .remove(&runtime_id)
        .ok_or_else(|| format!("runtime {runtime_id} does not exist"))?;
    Ok(())
}

#[tauri::command]
pub fn runtime_submit(
    runtime_id: String,
    draft: DesktopPromptDraft,
    registry: State<'_, RuntimeRegistry>,
) -> Result<DesktopSubmitDisposition, String> {
    registry.with_entry(&runtime_id, |entry| {
        entry.ensure_daemon()?;
        let (text, attachment_ids) = entry.stage_draft(draft)?;
        let command = if entry.session_id.is_none() {
            FrontendCommand::CreateSession {
                repository_profile: "desktop".to_owned(),
                objective: (!text.trim().is_empty()).then_some(text),
                attachment_ids,
            }
        } else {
            FrontendCommand::Submit {
                text,
                attachment_ids,
            }
        };
        let acknowledgement = entry.dispatch(command)?;
        let FrontendControlResult::SubmissionAccepted { session_id, queued } = acknowledgement.result
        else {
            return Err("daemon returned an unexpected submission result".to_owned());
        };
        entry.session_id = Some(session_id);
        Ok(if queued {
            DesktopSubmitDisposition::Queued
        } else {
            DesktopSubmitDisposition::Started
        })
    })
}

#[tauri::command]
pub fn runtime_command(
    runtime_id: String,
    input: String,
    registry: State<'_, RuntimeRegistry>,
) -> Result<(), String> {
    registry.with_entry(&runtime_id, |entry| {
        let command = if input.trim() == "/new" {
            FrontendCommand::NewSession
        } else {
            FrontendCommand::RunCommand { input }
        };
        let acknowledgement = entry.dispatch(command)?;
        if !matches!(
            &acknowledgement.result,
            FrontendControlResult::CommandAccepted { .. }
        ) {
            return Err("daemon returned an unexpected command result".to_owned());
        }
        if matches!(&acknowledgement.result, FrontendControlResult::CommandAccepted { command, .. } if command == "new_session") {
            entry.session_id = None;
            entry.replay_cursor = 0;
            entry.pending_ack_cursor = None;
            entry.presentation.reset();
        }
        Ok(())
    })
}

#[tauri::command]
pub fn runtime_command_suggestions(
    runtime_id: String,
    input: String,
    registry: State<'_, RuntimeRegistry>,
) -> Result<Vec<DesktopCommandSuggestion>, String> {
    registry.with_entry(&runtime_id, |entry| {
        Ok(command_suggestions(&input, &entry.repo)
            .into_iter()
            .map(|suggestion| DesktopCommandSuggestion {
                name: suggestion.name,
                usage: suggestion.usage,
                description: suggestion.description,
            })
            .collect())
    })
}

#[tauri::command]
pub fn runtime_cancel(
    runtime_id: String,
    registry: State<'_, RuntimeRegistry>,
) -> Result<bool, String> {
    registry.with_entry(&runtime_id, |entry| {
        let acknowledgement = entry.dispatch(FrontendCommand::CancelTurn)?;
        let FrontendControlResult::CancellationRequested { requested, .. } = acknowledgement.result
        else {
            return Err("daemon returned an unexpected cancellation result".to_owned());
        };
        Ok(requested)
    })
}

#[tauri::command]
pub fn runtime_poll(
    runtime_id: String,
    max_events: Option<usize>,
    registry: State<'_, RuntimeRegistry>,
) -> Result<Vec<DesktopRuntimeEvent>, String> {
    registry.with_entry(&runtime_id, |entry| {
        let mut events = Vec::new();
        let limit = max_events.unwrap_or(200).clamp(1, 500);
        if let Some(event) = entry.daemon_event() {
            if matches!(entry.daemon.last_state, Some(DaemonLifecycleState::Recovered)) {
                if let Some(session_id) = entry.session_id.clone() {
                    entry.resume(session_id)?;
                }
            }
            events.push(event);
        }
        entry.poll_daemon()?;
        while events.len() < limit {
            match entry.presentation.try_event() {
                Some(event) => events.push(event),
                None => break,
            }
        }
        Ok(events)
    })
}

fn verify_provider_route(
    prepared_profile: &crate::config::PreparedProviderProfile,
    provider: &str,
    model: &str,
    api_key: Option<&str>,
) -> Result<(), String> {
    let entry = provider_catalog_entry(provider)
        .ok_or_else(|| format!("unknown provider `{provider}`"))?;
    if entry.browser_oauth && !browser_oauth_credentials_present(provider) {
        return Err(format!(
            "{} is not authenticated. Sign in with ChatGPT before applying this provider.",
            entry.display_name
        ));
    }
    if !entry.discover_models {
        return Ok(());
    }

    let config = Config::load_layers_with_provider_profile(
        prepared_profile.profile(),
        None,
        None,
        &BTreeMap::new(),
        &BTreeMap::new(),
    )
    .map_err(|error| error.to_string())?;
    let discovered = discover_models(&config, api_key).map_err(|error| {
        format!(
            "{} route verification failed at {}: {error:?}",
            entry.display_name,
            prepared_profile
                .profile()
                .base_url
                .as_deref()
                .or(entry.base_url)
                .unwrap_or("the provider endpoint")
        )
    })?;
    if !discovered.iter().any(|candidate| candidate.id == model) {
        let available = discovered
            .iter()
            .map(|candidate| candidate.id.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        return Err(format!(
            "model `{model}` is not available from {}. Available models: {}",
            entry.display_name,
            if available.is_empty() { "none" } else { &available }
        ));
    }
    Ok(())
}

#[tauri::command]
pub fn runtime_configure_model(
    runtime_id: String,
    configuration: DesktopModelConfiguration,
    registry: State<'_, RuntimeRegistry>,
) -> Result<Option<DesktopConfigurationChanged>, String> {
    let effort_name = configuration.effort.to_ascii_lowercase();
    if !matches!(effort_name.as_str(), "low" | "medium" | "high" | "auto") {
        return Err("effort must be low, medium, high, or auto".to_owned());
    }
    let provider = configuration.provider;
    let model = configuration.model;
    let prepared_profile = prepare_provider_profile(
        &provider,
        &model,
        &effort_name,
        configuration.expected_revision,
        configuration.base_url.as_deref(),
    )?;
    let previous_profile = prepared_profile.previous_profile().clone();
    let profile_changed = prepared_profile.is_changed();
    let supplied_api_key = configuration.api_key.filter(|key| !key.trim().is_empty());
    let credentials = SystemCredentialStore;
    let api_key = match supplied_api_key.as_ref() {
        Some(api_key) => Some(api_key.clone()),
        None => credentials.load(&provider)?,
    };

    verify_provider_route(&prepared_profile, &provider, &model, api_key.as_deref())?;

    registry.with_entry(&runtime_id, |entry| {
        entry.sync_credential(&provider, api_key.clone())?;
        entry.dispatch(FrontendCommand::ConfigureModel {
            provider: Some(provider.clone()),
            model: model.clone(),
        })?;
        entry.dispatch(FrontendCommand::SetEffort {
            effort: effort_name.clone(),
        })?;
        Ok(())
    })?;
    let persisted = (|| {
        if let Some(api_key) = supplied_api_key {
            credentials.save(&provider, &api_key)?;
        }
        if profile_changed {
            prepared_profile.commit().map(Some)
        } else {
            drop(prepared_profile);
            Ok(None)
        }
    })();
    match persisted {
        Ok(change) => Ok(change.map(Into::into)),
        Err(error) => {
            restore_runtime_profile(&runtime_id, &previous_profile, &registry, &credentials)?;
            Err(error)
        }
    }
}

fn restore_runtime_profile(
    runtime_id: &str,
    profile: &medusa_config::ProviderProfile,
    registry: &State<'_, RuntimeRegistry>,
    credentials: &SystemCredentialStore,
) -> Result<(), String> {
    let effort = match profile.reasoning.as_str() {
        "low" => "low",
        "high" | "maximum" => "high",
        _ => "medium",
    };
    let api_key = credentials.load(&profile.provider)?;
    registry.with_entry(runtime_id, |entry| {
        entry.sync_credential(&profile.provider, api_key)?;
        entry.dispatch(FrontendCommand::ConfigureModel {
            provider: Some(profile.provider.clone()),
            model: profile.model.clone(),
        })?;
        entry.dispatch(FrontendCommand::SetEffort {
            effort: effort.to_owned(),
        })?;
        Ok(())
    })
}

fn canonical_directory(path: &Path) -> Result<PathBuf, String> {
    let canonical = fs::canonicalize(path)
        .map_err(|error| format!("cannot open {}: {error}", path.display()))?;
    if !canonical.is_dir() {
        return Err(format!("{} is not a directory", canonical.display()));
    }
    Ok(canonical)
}

fn checked_total(total: usize, additional: usize) -> Result<usize, String> {
    let total = total.saturating_add(additional);
    if total > MAX_TOTAL_ATTACHMENT_BYTES {
        return Err(format!(
            "prompt attachments total {total} bytes; limit is {MAX_TOTAL_ATTACHMENT_BYTES}"
        ));
    }
    Ok(total)
}

fn validate_image_dimensions(name: &str, width: u32, height: u32) -> Result<(), String> {
    if width == 0 || height == 0 {
        return Err(format!("image attachment {name} has zero dimensions"));
    }
    let pixels = u64::from(width)
        .checked_mul(u64::from(height))
        .ok_or_else(|| format!("image attachment {name} dimensions overflow"))?;
    if pixels > MAX_IMAGE_PIXELS {
        return Err(format!(
            "image attachment {name} has {pixels} pixels; limit is {MAX_IMAGE_PIXELS}"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oversized_image_dimensions_are_rejected_before_upload() {
        let error = validate_image_dimensions("bomb.png", 10_000, 10_000)
            .expect_err("oversized dimensions must fail");
        assert!(error.contains("pixels"));
    }

    #[test]
    fn desktop_client_identity_is_stable_for_cursor_reconnect() {
        assert_eq!(DESKTOP_CLIENT_ID, "desktop-primary");
    }
}
