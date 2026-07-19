use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use image::ImageReader;
use medusa_daemon::{DaemonLaunch, DaemonLifecycleState, DaemonSupervisor};
use medusa_runtime::{
    RuntimeController, SubmitDisposition,
    commands::{Effort, ModelConfiguration, parse_slash_command},
    prompt::{
        ClipboardImage, FileAttachment, MAX_CLIPBOARD_TEXT_BYTES, MAX_IMAGE_BYTES,
        MAX_IMAGE_PIXELS, MAX_TOTAL_ATTACHMENT_BYTES, PromptAttachment, PromptDraft,
        TextAttachment,
    },
};
use tauri::{AppHandle, Manager, State};

use crate::{
    credentials::{CredentialStore, SystemCredentialStore},
    dto::{
        DesktopAttachment, DesktopModelConfiguration, DesktopPromptDraft, DesktopRuntimeEvent,
        DesktopSubmitDisposition, RuntimeStartResponse,
    },
};

struct DesktopDaemon {
    supervisor: DaemonSupervisor,
    last_state: Option<DaemonLifecycleState>,
}

struct RuntimeEntry {
    repo: PathBuf,
    controller: RuntimeController,
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
}

impl Drop for RuntimeEntry {
    fn drop(&mut self) {
        self.controller.cancel();
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
        let controller = RuntimeController::start(repo.clone());
        let supervisor = DaemonLaunch::for_current_executable()
            .map(|launch| DaemonSupervisor::new(&repo, launch))
            .unwrap_or_else(|_| DaemonSupervisor::observe_only(&repo));
        let entry = Arc::new(Mutex::new(RuntimeEntry {
            repo: repo.clone(),
            controller,
            daemon: DesktopDaemon {
                supervisor,
                last_state: None,
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
        let draft = convert_prompt(&entry.repo, draft)?;
        entry
            .controller
            .submit(draft)
            .map(|disposition| match disposition {
                SubmitDisposition::Started => DesktopSubmitDisposition::Started,
                SubmitDisposition::Queued => DesktopSubmitDisposition::Queued,
            })
            .map_err(|error| error.to_string())
    })
}

#[tauri::command]
pub fn runtime_command(
    runtime_id: String,
    input: String,
    registry: State<'_, RuntimeRegistry>,
) -> Result<(), String> {
    let command = parse_slash_command(&input)
        .map_err(|error| format!("invalid slash command: {error}"))?
        .ok_or_else(|| "runtime_command expects a slash command".to_owned())?;
    registry.with_entry(&runtime_id, |entry| {
        entry
            .controller
            .run_command(command)
            .map_err(|error| error.to_string())
    })
}

#[tauri::command]
pub fn runtime_cancel(
    runtime_id: String,
    registry: State<'_, RuntimeRegistry>,
) -> Result<bool, String> {
    registry.with_entry(&runtime_id, |entry| Ok(entry.controller.cancel()))
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
            events.push(event);
        }
        while events.len() < limit {
            match entry
                .controller
                .try_event()
                .map_err(|error| error.to_string())?
            {
                Some(event) => events.push(event.into()),
                None => break,
            }
        }
        Ok(events)
    })
}

#[tauri::command]
pub fn runtime_configure_model(
    runtime_id: String,
    configuration: DesktopModelConfiguration,
    registry: State<'_, RuntimeRegistry>,
) -> Result<(), String> {
    let effort = match configuration.effort.to_ascii_lowercase().as_str() {
        "low" => Effort::Low,
        "medium" => Effort::Medium,
        "high" => Effort::High,
        "auto" => Effort::Auto,
        _ => return Err("effort must be low, medium, high, or auto".to_owned()),
    };
    let provider = configuration.provider;
    let supplied_api_key = configuration.api_key.filter(|key| !key.trim().is_empty());
    let credentials = SystemCredentialStore;
    let api_key = match supplied_api_key.as_ref() {
        Some(api_key) => Some(api_key.clone()),
        None => credentials.load(&provider)?,
    };
    registry.with_entry(&runtime_id, |entry| {
        entry
            .controller
            .configure_model(ModelConfiguration {
                provider: provider.clone(),
                model: configuration.model,
                effort,
                api_key,
            })
            .map_err(|error| error.to_string())
    })?;
    if let Some(api_key) = supplied_api_key {
        credentials.save(&provider, &api_key)?;
    }
    Ok(())
}

fn canonical_directory(path: &Path) -> Result<PathBuf, String> {
    let canonical = fs::canonicalize(path)
        .map_err(|error| format!("cannot open {}: {error}", path.display()))?;
    if !canonical.is_dir() {
        return Err(format!("{} is not a directory", canonical.display()));
    }
    Ok(canonical)
}

fn convert_prompt(repo: &Path, source: DesktopPromptDraft) -> Result<PromptDraft, String> {
    let mut draft = PromptDraft {
        text: source.text,
        attachments: Vec::new(),
        revision: source.revision,
    };
    for attachment in source.attachments {
        match attachment {
            DesktopAttachment::File { path } => attach_file(repo, &mut draft, Path::new(&path))?,
            DesktopAttachment::Image { name, data_url } => {
                attach_image(&mut draft, &name, &data_url)?;
            }
            DesktopAttachment::Text { name, text } => attach_text(&mut draft, name, text)?,
        }
    }
    Ok(draft)
}

fn attach_file(repo: &Path, draft: &mut PromptDraft, path: &Path) -> Result<(), String> {
    let canonical = fs::canonicalize(path)
        .map_err(|error| format!("cannot attach {}: {error}", path.display()))?;
    if !canonical.starts_with(repo) {
        return Err(format!(
            "attachment {} is outside the selected repository",
            canonical.display()
        ));
    }
    let metadata = fs::metadata(&canonical)
        .map_err(|error| format!("cannot inspect {}: {error}", canonical.display()))?;
    if !metadata.is_file() {
        return Err(format!("attachment {} is not a file", canonical.display()));
    }
    let byte_len = usize::try_from(metadata.len())
        .map_err(|_| format!("attachment {} is too large", canonical.display()))?;
    ensure_total(draft, byte_len)?;
    draft
        .attachments
        .push(PromptAttachment::File(FileAttachment {
            path: canonical,
            byte_len,
        }));
    Ok(())
}

fn attach_text(draft: &mut PromptDraft, name: String, text: String) -> Result<(), String> {
    if text.len() > MAX_CLIPBOARD_TEXT_BYTES {
        return Err(format!(
            "text attachment {name} exceeds the clipboard text limit"
        ));
    }
    ensure_total(draft, text.len())?;
    draft
        .attachments
        .push(PromptAttachment::PastedText(TextAttachment {
            display_name: name,
            text,
        }));
    Ok(())
}

fn attach_image(draft: &mut PromptDraft, name: &str, data_url: &str) -> Result<(), String> {
    let (header, encoded) = data_url
        .split_once(',')
        .ok_or_else(|| format!("image attachment {name} is not a data URL"))?;
    if !header.starts_with("data:image/") || !header.ends_with(";base64") {
        return Err(format!(
            "image attachment {name} must be a base64 image data URL"
        ));
    }
    let max_encoded_bytes = MAX_IMAGE_BYTES
        .saturating_mul(4)
        .div_ceil(3)
        .saturating_add(4);
    if encoded.len() > max_encoded_bytes {
        return Err(format!(
            "encoded image attachment {name} exceeds the {MAX_IMAGE_BYTES}-byte image limit"
        ));
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
    validate_image_dimensions(name, dimensions.0, dimensions.1)?;
    let image = ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|error| format!("cannot detect image attachment {name}: {error}"))?
        .decode()
        .map_err(|error| format!("cannot decode image attachment {name}: {error}"))?;
    let rgba = image.to_rgba8();
    draft
        .add_image(ClipboardImage {
            width: rgba.width(),
            height: rgba.height(),
            rgba: rgba.into_raw(),
            source_format: Some(
                header
                    .trim_start_matches("data:")
                    .trim_end_matches(";base64")
                    .to_owned(),
            ),
        })
        .map_err(|error| error.to_string())?;
    if let Some(PromptAttachment::Image(image)) = draft.attachments.last_mut() {
        image.display_name = name.to_owned();
    }
    Ok(())
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
    let rgba_bytes = pixels
        .checked_mul(4)
        .ok_or_else(|| format!("image attachment {name} byte count overflow"))?;
    if rgba_bytes > MAX_IMAGE_BYTES as u64 {
        return Err(format!(
            "image attachment {name} requires {rgba_bytes} RGBA bytes; limit is {MAX_IMAGE_BYTES}"
        ));
    }
    Ok(())
}

fn ensure_total(draft: &PromptDraft, additional: usize) -> Result<(), String> {
    let total = draft.total_attachment_bytes().saturating_add(additional);
    if total > MAX_TOTAL_ATTACHMENT_BYTES {
        return Err(format!(
            "prompt attachments total {total} bytes; limit is {MAX_TOTAL_ATTACHMENT_BYTES}"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn file_attachments_are_confined_to_the_selected_repository() {
        let repo = tempdir().expect("repo");
        let outside = tempdir().expect("outside");
        let path = outside.path().join("secret.txt");
        fs::write(&path, "secret").expect("write outside file");
        let error = convert_prompt(
            repo.path(),
            DesktopPromptDraft {
                text: String::new(),
                attachments: vec![DesktopAttachment::File {
                    path: path.to_string_lossy().into_owned(),
                }],
                revision: 0,
            },
        )
        .expect_err("outside attachment must fail");
        assert!(error.contains("outside the selected repository"));
    }

    #[test]
    fn oversized_image_dimensions_are_rejected_before_decode() {
        let error = validate_image_dimensions("bomb.png", 10_000, 10_000)
            .expect_err("oversized dimensions must fail");
        assert!(error.contains("pixels"));
    }

    #[test]
    fn repository_file_attachment_keeps_canonical_path_and_size() {
        let repo = tempdir().expect("repo");
        let path = repo.path().join("context.txt");
        fs::write(&path, "context").expect("write file");
        let draft = convert_prompt(
            repo.path(),
            DesktopPromptDraft {
                text: "review this".to_owned(),
                attachments: vec![DesktopAttachment::File {
                    path: path.to_string_lossy().into_owned(),
                }],
                revision: 4,
            },
        )
        .expect("valid attachment");
        assert_eq!(draft.revision, 4);
        assert!(
            matches!(&draft.attachments[0], PromptAttachment::File(file) if file.byte_len == 7)
        );
    }
}
