use std::{
    collections::BTreeMap,
    fs, io,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock, Weak},
    thread,
    time::Duration,
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::clipboard::{
    FileAttachment, ImageAttachment, PromptAttachment, PromptDraft, TextAttachment,
};

const MANIFEST_NAME: &str = "draft.json";
const ATTACHMENTS_DIR: &str = "attachments";
const MAX_DRAFT_KEY_LEN: usize = 128;
const DRAFT_WRITE_DEBOUNCE: Duration = Duration::from_millis(75);

static DRAFT_WRITERS: OnceLock<Mutex<BTreeMap<PathBuf, Weak<DraftWriter>>>> = OnceLock::new();

#[derive(Clone, Debug)]
pub struct DraftStore {
    root: PathBuf,
    writer: Arc<DraftWriter>,
}

#[derive(Debug, Default)]
struct DraftWriter {
    state: Mutex<WriterState>,
    io_lock: Mutex<()>,
}

#[derive(Debug, Default)]
struct WriterState {
    next_generation: u64,
    pending: BTreeMap<String, PendingWrite>,
    in_flight: BTreeMap<String, PendingWrite>,
    current: BTreeMap<String, u64>,
    worker_running: bool,
    last_error: Option<StoredIoError>,
}

#[derive(Clone, Debug)]
struct PendingWrite {
    generation: u64,
    draft: PromptDraft,
}

#[derive(Debug)]
struct StoredIoError {
    kind: io::ErrorKind,
    message: String,
}

impl DraftStore {
    #[must_use]
    pub fn for_repo(repo: &Path) -> Self {
        let root = repo.join(".medusa/drafts");
        Self {
            writer: shared_writer(&root),
            root,
        }
    }

    pub fn save(&self, key: &str, draft: &PromptDraft) -> io::Result<()> {
        validate_key(key)?;
        self.take_background_error()?;

        let should_spawn = {
            let mut state = lock(&self.writer.state);
            state.next_generation = state.next_generation.saturating_add(1);
            let generation = state.next_generation;
            state.current.insert(key.to_owned(), generation);
            match state.pending.get_mut(key) {
                Some(pending) => {
                    pending.generation = generation;
                    pending.draft.text.clone_from(&draft.text);
                    pending.draft.revision = draft.revision;
                    if pending.draft.attachments != draft.attachments {
                        pending.draft.attachments.clone_from(&draft.attachments);
                    }
                }
                None => {
                    state.pending.insert(
                        key.to_owned(),
                        PendingWrite {
                            generation,
                            draft: draft.clone(),
                        },
                    );
                }
            }
            if state.worker_running {
                false
            } else {
                state.worker_running = true;
                true
            }
        };

        if should_spawn {
            self.spawn_writer()?;
        }
        Ok(())
    }

    pub fn flush(&self) -> io::Result<()> {
        self.take_background_error()?;
        let _io_guard = lock(&self.writer.io_lock);
        let writes = {
            let mut state = lock(&self.writer.state);
            let mut writes = std::mem::take(&mut state.in_flight);
            for (key, pending) in std::mem::take(&mut state.pending) {
                match writes.get(&key) {
                    Some(existing) if existing.generation >= pending.generation => {}
                    _ => {
                        writes.insert(key, pending);
                    }
                }
            }
            writes
        };
        write_batch(&self.root, &self.writer, writes)
    }

    pub fn load(&self, key: &str) -> io::Result<Option<PromptDraft>> {
        validate_key(key)?;
        if key == "current" {
            self.delete(key)?;
            return Ok(None);
        }
        self.flush()?;
        let directory = self.root.join(key);
        let manifest_path = directory.join(MANIFEST_NAME);
        if !manifest_path.exists() {
            return Ok(None);
        }
        let manifest: StoredDraft =
            serde_json::from_slice(&fs::read(&manifest_path)?).map_err(json_error)?;
        let mut attachments = Vec::with_capacity(manifest.attachments.len());
        for attachment in manifest.attachments {
            attachments.push(match attachment {
                StoredAttachment::PastedText { display_name, text } => {
                    PromptAttachment::PastedText(TextAttachment { display_name, text })
                }
                StoredAttachment::Image {
                    display_name,
                    width,
                    height,
                    source_format,
                    file_name,
                    byte_len,
                    sha256,
                } => {
                    validate_attachment_name(&file_name)?;
                    let bytes = fs::read(directory.join(ATTACHMENTS_DIR).join(file_name))?;
                    if bytes.len() != byte_len || digest_hex(&bytes) != sha256 {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "draft image attachment failed integrity verification",
                        ));
                    }
                    PromptAttachment::Image(ImageAttachment {
                        display_name,
                        width,
                        height,
                        rgba: bytes,
                        source_format,
                    })
                }
                StoredAttachment::File { path, byte_len } => {
                    PromptAttachment::File(FileAttachment { path, byte_len })
                }
            });
        }
        Ok(Some(PromptDraft {
            text: manifest.text,
            attachments,
            revision: manifest.revision,
        }))
    }

    pub fn delete(&self, key: &str) -> io::Result<()> {
        validate_key(key)?;
        self.take_background_error()?;
        {
            let mut state = lock(&self.writer.state);
            state.next_generation = state.next_generation.saturating_add(1);
            state.pending.remove(key);
            state.in_flight.remove(key);
            state.current.remove(key);
        }
        let _io_guard = lock(&self.writer.io_lock);
        let directory = self.root.join(key);
        if directory.exists() {
            fs::remove_dir_all(directory)?;
        }
        Ok(())
    }

    fn spawn_writer(&self) -> io::Result<()> {
        let root = self.root.clone();
        let writer = Arc::clone(&self.writer);
        if let Err(error) = thread::Builder::new()
            .name("medusa-draft-writer".to_owned())
            .spawn(move || writer_loop(root, writer))
        {
            lock(&self.writer.state).worker_running = false;
            return Err(io::Error::other(format!(
                "failed to start draft writer: {error}"
            )));
        }
        Ok(())
    }

    fn take_background_error(&self) -> io::Result<()> {
        let error = lock(&self.writer.state).last_error.take();
        match error {
            Some(error) => Err(io::Error::new(error.kind, error.message)),
            None => Ok(()),
        }
    }
}

fn shared_writer(root: &Path) -> Arc<DraftWriter> {
    let writers = DRAFT_WRITERS.get_or_init(|| Mutex::new(BTreeMap::new()));
    let mut writers = lock(writers);
    writers.retain(|_, writer| writer.strong_count() > 0);
    if let Some(writer) = writers.get(root).and_then(Weak::upgrade) {
        return writer;
    }
    let writer = Arc::new(DraftWriter::default());
    writers.insert(root.to_path_buf(), Arc::downgrade(&writer));
    writer
}

fn writer_loop(root: PathBuf, writer: Arc<DraftWriter>) {
    loop {
        thread::sleep(DRAFT_WRITE_DEBOUNCE);
        {
            let mut state = lock(&writer.state);
            if state.pending.is_empty() {
                state.worker_running = false;
                return;
            }
            let pending = std::mem::take(&mut state.pending);
            for (key, write) in pending {
                match state.in_flight.get(&key) {
                    Some(existing) if existing.generation >= write.generation => {}
                    _ => {
                        state.in_flight.insert(key, write);
                    }
                }
            }
        }

        let result = {
            let _io_guard = lock(&writer.io_lock);
            let writes = std::mem::take(&mut lock(&writer.state).in_flight);
            write_batch(&root, &writer, writes)
        };
        if let Err(error) = result {
            lock(&writer.state).last_error = Some(StoredIoError {
                kind: error.kind(),
                message: error.to_string(),
            });
        }
    }
}

fn write_batch(
    root: &Path,
    writer: &DraftWriter,
    writes: BTreeMap<String, PendingWrite>,
) -> io::Result<()> {
    for (key, pending) in writes {
        let is_current = lock(&writer.state).current.get(&key).copied() == Some(pending.generation);
        if is_current {
            write_draft(root, &key, &pending.draft)?;
        }
    }
    Ok(())
}

fn write_draft(root: &Path, key: &str, draft: &PromptDraft) -> io::Result<()> {
    let directory = root.join(key);
    let attachments_directory = directory.join(ATTACHMENTS_DIR);
    fs::create_dir_all(&attachments_directory)?;

    let mut stored_attachments = Vec::with_capacity(draft.attachments.len());
    for (index, attachment) in draft.attachments.iter().enumerate() {
        stored_attachments.push(match attachment {
            PromptAttachment::PastedText(text) => StoredAttachment::PastedText {
                display_name: text.display_name.clone(),
                text: text.text.clone(),
            },
            PromptAttachment::Image(image) => {
                let file_name = format!("image-{index}.rgba");
                let path = attachments_directory.join(&file_name);
                atomic_write(&path, &image.rgba)?;
                StoredAttachment::Image {
                    display_name: image.display_name.clone(),
                    width: image.width,
                    height: image.height,
                    source_format: image.source_format.clone(),
                    file_name,
                    byte_len: image.rgba.len(),
                    sha256: digest_hex(&image.rgba),
                }
            }
            PromptAttachment::File(file) => StoredAttachment::File {
                path: file.path.clone(),
                byte_len: file.byte_len,
            },
        });
    }

    let manifest = StoredDraft {
        text: draft.text.clone(),
        revision: draft.revision,
        attachments: stored_attachments,
    };
    let encoded = serde_json::to_vec_pretty(&manifest).map_err(json_error)?;
    atomic_write(&directory.join(MANIFEST_NAME), &encoded)
}

#[derive(Debug, Deserialize, Serialize)]
struct StoredDraft {
    text: String,
    revision: u64,
    attachments: Vec<StoredAttachment>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum StoredAttachment {
    PastedText {
        display_name: String,
        text: String,
    },
    Image {
        display_name: String,
        width: u32,
        height: u32,
        source_format: Option<String>,
        file_name: String,
        byte_len: usize,
        sha256: String,
    },
    File {
        path: PathBuf,
        byte_len: usize,
    },
}

fn validate_key(key: &str) -> io::Result<()> {
    let valid = !key.is_empty()
        && key.len() <= MAX_DRAFT_KEY_LEN
        && key
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'));
    if valid {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "draft key must contain only ASCII letters, digits, '-' or '_'",
        ))
    }
}

fn validate_attachment_name(name: &str) -> io::Result<()> {
    if Path::new(name).components().count() == 1 && !name.is_empty() {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "draft attachment path is not contained",
        ))
    }
}

fn atomic_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let parent = path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "target has no parent directory",
        )
    })?;
    fs::create_dir_all(parent)?;
    let temporary = path.with_extension(format!("tmp-{}", std::process::id()));
    fs::write(&temporary, bytes)?;
    fs::rename(temporary, path)
}

fn digest_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn json_error(error: serde_json::Error) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error)
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn draft_round_trip_preserves_text_and_image() {
        let repository = tempdir().expect("temporary repository");
        let store = DraftStore::for_repo(repository.path());
        let draft = PromptDraft {
            text: "fix the screenshot issue".to_owned(),
            attachments: vec![PromptAttachment::Image(ImageAttachment {
                display_name: "screenshot-1.png".to_owned(),
                width: 2,
                height: 1,
                rgba: vec![1, 2, 3, 4, 5, 6, 7, 8],
                source_format: Some("image/png".to_owned()),
            })],
            revision: 4,
        };

        store.save("session_123", &draft).expect("save draft");
        assert_eq!(store.load("session_123").expect("load draft"), Some(draft));
    }

    #[test]
    fn rapid_updates_are_deferred_and_coalesced() {
        let repository = tempdir().expect("temporary repository");
        let store = DraftStore::for_repo(repository.path());
        let mut draft = PromptDraft {
            text: "h".to_owned(),
            revision: 1,
            ..PromptDraft::default()
        };
        store.save("current", &draft).expect("queue first draft");
        assert!(
            !repository
                .path()
                .join(".medusa/drafts/current/draft.json")
                .exists(),
            "save must return before filesystem persistence"
        );

        draft.text = "hey".to_owned();
        draft.revision = 3;
        store.save("current", &draft).expect("queue latest draft");
        store.flush().expect("flush latest draft");

        let reopened = DraftStore::for_repo(repository.path());
        assert_eq!(reopened.load("current").expect("load persisted draft"), None);
        assert!(!repository.path().join(".medusa/drafts/current").exists());
    }

    #[test]
    fn independent_handles_share_pending_writes() {
        let repository = tempdir().expect("temporary repository");
        let writer = DraftStore::for_repo(repository.path());
        writer
            .save(
                "session_1",
                &PromptDraft {
                    text: "shared pending draft".to_owned(),
                    ..PromptDraft::default()
                },
            )
            .expect("queue draft");

        let reader = DraftStore::for_repo(repository.path());
        assert_eq!(
            reader
                .load("session_1")
                .expect("load shared draft")
                .expect("draft exists")
                .text,
            "shared pending draft"
        );
    }

    #[test]
    fn delete_cancels_a_delayed_write() {
        let repository = tempdir().expect("temporary repository");
        let store = DraftStore::for_repo(repository.path());
        store
            .save(
                "current",
                &PromptDraft {
                    text: "do not recreate me".to_owned(),
                    ..PromptDraft::default()
                },
            )
            .expect("queue draft");
        store.delete("current").expect("delete pending draft");
        thread::sleep(DRAFT_WRITE_DEBOUNCE + DRAFT_WRITE_DEBOUNCE);
        assert!(!repository.path().join(".medusa/drafts/current").exists());
    }

    #[test]
    fn traversal_key_is_rejected() {
        let repository = tempdir().expect("temporary repository");
        let store = DraftStore::for_repo(repository.path());
        assert!(store.load("../escape").is_err());
    }

    #[test]
    fn tampered_image_is_rejected() {
        let repository = tempdir().expect("temporary repository");
        let store = DraftStore::for_repo(repository.path());
        let draft = PromptDraft {
            attachments: vec![PromptAttachment::Image(ImageAttachment {
                display_name: "screenshot-1.png".to_owned(),
                width: 1,
                height: 1,
                rgba: vec![0, 0, 0, 0],
                source_format: Some("image/png".to_owned()),
            })],
            ..PromptDraft::default()
        };
        store.save("session_1", &draft).expect("save draft");
        store.flush().expect("flush draft");
        fs::write(
            repository
                .path()
                .join(".medusa/drafts/session_1/attachments/image-0.rgba"),
            [9, 9, 9, 9],
        )
        .expect("tamper attachment");
        let error = store
            .load("session_1")
            .expect_err("integrity check must fail");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }
}
