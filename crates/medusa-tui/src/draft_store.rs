use std::{
    fs,
    io,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::clipboard::{
    FileAttachment, ImageAttachment, PromptAttachment, PromptDraft, TextAttachment,
};

const MANIFEST_NAME: &str = "draft.json";
const ATTACHMENTS_DIR: &str = "attachments";
const MAX_DRAFT_KEY_LEN: usize = 128;

#[derive(Clone, Debug)]
pub struct DraftStore {
    root: PathBuf,
}

impl DraftStore {
    #[must_use]
    pub fn for_repo(repo: &Path) -> Self {
        Self {
            root: repo.join(".medusa/drafts"),
        }
    }

    pub fn save(&self, key: &str, draft: &PromptDraft) -> io::Result<()> {
        validate_key(key)?;
        let directory = self.root.join(key);
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

    pub fn load(&self, key: &str) -> io::Result<Option<PromptDraft>> {
        validate_key(key)?;
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
        let directory = self.root.join(key);
        if directory.exists() {
            fs::remove_dir_all(directory)?;
        }
        Ok(())
    }
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
        io::Error::new(io::ErrorKind::InvalidInput, "target has no parent directory")
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
        assert_eq!(
            store.load("session_123").expect("load draft"),
            Some(draft)
        );
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
