//! Repository-scoped staging store for frontend prompt attachments.
//!
//! Frontends ingest bounded bytes and receive opaque content-addressed identifiers. The shared
//! control plane resolves those identifiers into the runtime's canonical prompt attachment model
//! immediately before submission. No frontend-supplied path is ever trusted.

use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

use medusa_runtime::{
    attachment::{
        MAX_CLIPBOARD_TEXT_BYTES, MAX_IMAGE_BYTES, MAX_IMAGES_PER_PROMPT,
        MAX_TOTAL_ATTACHMENT_BYTES,
    },
    prompt::{FileAttachment, PromptAttachment, TextAttachment},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::protocol::FrontendArtifactKind;

const ARTIFACT_SCHEMA_VERSION: u32 = 1;
const ARTIFACT_PREFIX: &str = "frontend-artifact-";
const MAX_DISPLAY_NAME_CHARS: usize = 240;
const MAX_MIME_TYPE_CHARS: usize = 128;

#[derive(Clone, Debug)]
pub struct FrontendArtifactInput {
    pub display_name: String,
    pub mime_type: Option<String>,
    pub kind: FrontendArtifactKind,
    pub bytes: Vec<u8>,
}

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
pub struct FrontendArtifactExport {
    pub display_name: String,
    pub mime_type: Option<String>,
    pub kind: FrontendArtifactKind,
    pub bytes: Vec<u8>,
}

impl std::fmt::Debug for FrontendArtifactExport {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("FrontendArtifactExport")
            .field("display_name", &self.display_name)
            .field("mime_type", &self.mime_type)
            .field("kind", &self.kind)
            .field("bytes", &format_args!("<{} bytes>", self.bytes.len()))
            .finish()
    }
}

#[derive(Clone, Debug)]
pub struct FrontendArtifactStore {
    root: PathBuf,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct FrontendArtifactMetadata {
    schema_version: u32,
    artifact_id: String,
    display_name: String,
    mime_type: Option<String>,
    #[serde(default)]
    kind: Option<FrontendArtifactKind>,
    byte_len: usize,
    sha256: String,
}

impl FrontendArtifactStore {
    #[must_use]
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    pub fn ingest(
        &self,
        input: FrontendArtifactInput,
    ) -> Result<String, FrontendArtifactStoreError> {
        let display_name = validate_display_name(&input.display_name)?;
        let mime_type = validate_mime_type(input.mime_type.as_deref())?;
        if input.bytes.is_empty() {
            return Err(FrontendArtifactStoreError::EmptyArtifact);
        }
        let byte_limit = match input.kind {
            FrontendArtifactKind::Image => MAX_IMAGE_BYTES,
            FrontendArtifactKind::Text => MAX_CLIPBOARD_TEXT_BYTES,
            FrontendArtifactKind::File => MAX_TOTAL_ATTACHMENT_BYTES,
        };
        if input.bytes.len() > byte_limit {
            return Err(FrontendArtifactStoreError::ByteLimit {
                bytes: input.bytes.len(),
                limit: byte_limit,
            });
        }
        if input.kind == FrontendArtifactKind::Image
            && !is_supported_image_mime(mime_type.as_deref())
        {
            return Err(FrontendArtifactStoreError::UnsupportedImageMimeType);
        }

        let byte_digest = hex::encode(Sha256::digest(&input.bytes));
        let mut identity = Sha256::new();
        identity.update(display_name.as_bytes());
        identity.update([0]);
        identity.update(mime_type.as_deref().unwrap_or_default().as_bytes());
        identity.update([0]);
        identity.update(format!("{:?}", input.kind).as_bytes());
        identity.update([0]);
        identity.update(byte_digest.as_bytes());
        let artifact_digest = hex::encode(identity.finalize());
        let artifact_id = format!("{ARTIFACT_PREFIX}{artifact_digest}");
        let metadata = FrontendArtifactMetadata {
            schema_version: ARTIFACT_SCHEMA_VERSION,
            artifact_id: artifact_id.clone(),
            display_name,
            mime_type,
            kind: Some(input.kind),
            byte_len: input.bytes.len(),
            sha256: byte_digest,
        };
        fs::create_dir_all(self.artifact_dir(&artifact_digest))?;
        write_once(
            &self.blob_path(&artifact_digest, &metadata.display_name),
            &input.bytes,
        )?;
        write_once(
            &self.metadata_path(&artifact_digest),
            &serde_json::to_vec_pretty(&metadata)?,
        )?;
        Ok(artifact_id)
    }

    pub fn resolve(
        &self,
        artifact_ids: &[String],
    ) -> Result<Vec<PromptAttachment>, FrontendArtifactStoreError> {
        let mut total_bytes = 0_usize;
        let mut image_count = 0_usize;
        let mut attachments = Vec::with_capacity(artifact_ids.len());
        for artifact_id in artifact_ids {
            let (metadata, blob_path, bytes) = self.read_verified(artifact_id)?;
            total_bytes = total_bytes.saturating_add(bytes.len());
            let kind = metadata
                .kind
                .unwrap_or_else(|| infer_legacy_kind(metadata.mime_type.as_deref()));
            if kind == FrontendArtifactKind::Image {
                image_count = image_count.saturating_add(1);
                if image_count > MAX_IMAGES_PER_PROMPT {
                    return Err(FrontendArtifactStoreError::ImageCountLimit(
                        MAX_IMAGES_PER_PROMPT,
                    ));
                }
            }
            if total_bytes > MAX_TOTAL_ATTACHMENT_BYTES {
                return Err(FrontendArtifactStoreError::ByteLimit {
                    bytes: total_bytes,
                    limit: MAX_TOTAL_ATTACHMENT_BYTES,
                });
            }
            attachments.push(resolve_attachment(kind, metadata, blob_path, bytes)?);
        }
        Ok(attachments)
    }

    #[allow(dead_code)]
    pub fn export(
        &self,
        artifact_id: &str,
    ) -> Result<FrontendArtifactExport, FrontendArtifactStoreError> {
        let (metadata, _, bytes) = self.read_verified(artifact_id)?;
        let kind = metadata
            .kind
            .unwrap_or_else(|| infer_legacy_kind(metadata.mime_type.as_deref()));
        Ok(FrontendArtifactExport {
            display_name: metadata.display_name,
            mime_type: metadata.mime_type,
            kind,
            bytes,
        })
    }

    fn read_verified(
        &self,
        artifact_id: &str,
    ) -> Result<(FrontendArtifactMetadata, PathBuf, Vec<u8>), FrontendArtifactStoreError> {
        let digest = parse_artifact_id(artifact_id)?;
        let metadata: FrontendArtifactMetadata =
            serde_json::from_slice(&fs::read(self.metadata_path(digest))?)?;
        if metadata.schema_version != ARTIFACT_SCHEMA_VERSION || metadata.artifact_id != artifact_id
        {
            return Err(FrontendArtifactStoreError::CorruptArtifact(
                artifact_id.to_owned(),
            ));
        }
        let blob_path = self.blob_path(digest, &metadata.display_name);
        let bytes = fs::read(&blob_path)?;
        if bytes.len() != metadata.byte_len
            || hex::encode(Sha256::digest(&bytes)) != metadata.sha256
        {
            return Err(FrontendArtifactStoreError::CorruptArtifact(
                artifact_id.to_owned(),
            ));
        }
        Ok((metadata, blob_path, bytes))
    }

    fn metadata_path(&self, digest: &str) -> PathBuf {
        self.root.join(format!("{digest}.json"))
    }

    fn artifact_dir(&self, digest: &str) -> PathBuf {
        self.root.join(digest)
    }

    fn blob_path(&self, digest: &str, display_name: &str) -> PathBuf {
        self.artifact_dir(digest).join(display_name)
    }
}

fn resolve_attachment(
    kind: FrontendArtifactKind,
    metadata: FrontendArtifactMetadata,
    blob_path: PathBuf,
    bytes: Vec<u8>,
) -> Result<PromptAttachment, FrontendArtifactStoreError> {
    match kind {
        FrontendArtifactKind::Image | FrontendArtifactKind::File => {
            Ok(PromptAttachment::File(FileAttachment {
                path: blob_path,
                byte_len: bytes.len(),
            }))
        }
        FrontendArtifactKind::Text => {
            let text = String::from_utf8(bytes).map_err(|_| {
                FrontendArtifactStoreError::UnsupportedBinaryArtifact(metadata.display_name.clone())
            })?;
            Ok(PromptAttachment::PastedText(TextAttachment {
                display_name: metadata.display_name,
                text,
            }))
        }
    }
}

fn infer_legacy_kind(mime_type: Option<&str>) -> FrontendArtifactKind {
    if is_supported_image_mime(mime_type) {
        FrontendArtifactKind::Image
    } else {
        FrontendArtifactKind::Text
    }
}

fn is_supported_image_mime(value: Option<&str>) -> bool {
    matches!(
        value,
        Some("image/gif" | "image/jpeg" | "image/png" | "image/webp")
    )
}

fn parse_artifact_id(artifact_id: &str) -> Result<&str, FrontendArtifactStoreError> {
    let digest = artifact_id
        .strip_prefix(ARTIFACT_PREFIX)
        .ok_or_else(|| FrontendArtifactStoreError::InvalidArtifactId(artifact_id.to_owned()))?;
    if digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(FrontendArtifactStoreError::InvalidArtifactId(
            artifact_id.to_owned(),
        ));
    }
    Ok(digest)
}

fn validate_display_name(value: &str) -> Result<String, FrontendArtifactStoreError> {
    let trimmed = value.trim();
    if trimmed.is_empty()
        || trimmed.chars().count() > MAX_DISPLAY_NAME_CHARS
        || trimmed
            .chars()
            .any(|character| character.is_control() || matches!(character, '/' | '\\'))
    {
        return Err(FrontendArtifactStoreError::InvalidDisplayName);
    }
    Ok(trimmed.to_owned())
}

fn validate_mime_type(value: Option<&str>) -> Result<Option<String>, FrontendArtifactStoreError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let trimmed = value.trim().to_ascii_lowercase();
    if trimmed.is_empty()
        || trimmed.len() > MAX_MIME_TYPE_CHARS
        || !trimmed
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'+' | b'-' | b'.'))
    {
        return Err(FrontendArtifactStoreError::InvalidMimeType);
    }
    Ok(Some(trimmed))
}

fn write_once(path: &Path, bytes: &[u8]) -> Result<(), FrontendArtifactStoreError> {
    match OpenOptions::new().create_new(true).write(true).open(path) {
        Ok(mut file) => {
            file.write_all(bytes)?;
            file.sync_all()?;
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(error) => Err(error.into()),
    }
}

#[derive(Debug, Error)]
pub enum FrontendArtifactStoreError {
    #[error("frontend artifact display name is invalid")]
    InvalidDisplayName,
    #[error("frontend artifact MIME type is invalid")]
    InvalidMimeType,
    #[error("frontend image MIME type is not supported")]
    UnsupportedImageMimeType,
    #[error("frontend artifact is empty")]
    EmptyArtifact,
    #[error("frontend artifact is {bytes} bytes; limit is {limit}")]
    ByteLimit { bytes: usize, limit: usize },
    #[error("frontend prompt allows at most {0} images")]
    ImageCountLimit(usize),
    #[error("frontend artifact id is invalid: {0}")]
    InvalidArtifactId(String),
    #[error("frontend artifact is corrupt: {0}")]
    CorruptArtifact(String),
    #[error("frontend binary artifact is not a supported image or UTF-8 text file: {0}")]
    UnsupportedBinaryArtifact(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_artifact_is_content_addressed_and_resolves() {
        let directory = tempfile::tempdir().expect("artifact store");
        let store = FrontendArtifactStore::new(directory.path().to_path_buf());
        let id = store
            .ingest(FrontendArtifactInput {
                display_name: "notes.txt".to_owned(),
                mime_type: Some("text/plain".to_owned()),
                kind: FrontendArtifactKind::Text,
                bytes: b"hello desktop".to_vec(),
            })
            .expect("ingest");
        let duplicate = store
            .ingest(FrontendArtifactInput {
                display_name: "notes.txt".to_owned(),
                mime_type: Some("text/plain".to_owned()),
                kind: FrontendArtifactKind::Text,
                bytes: b"hello desktop".to_vec(),
            })
            .expect("duplicate ingest");
        assert_eq!(id, duplicate);
        assert!(matches!(
            store.resolve(&[id]).expect("resolve").as_slice(),
            [PromptAttachment::PastedText(TextAttachment { text, .. })] if text == "hello desktop"
        ));
    }

    #[test]
    fn binary_file_resolves_to_repository_scoped_staging_path() {
        let directory = tempfile::tempdir().expect("artifact store");
        let store = FrontendArtifactStore::new(directory.path().to_path_buf());
        let id = store
            .ingest(FrontendArtifactInput {
                display_name: "context.bin".to_owned(),
                mime_type: Some("application/octet-stream".to_owned()),
                kind: FrontendArtifactKind::File,
                bytes: vec![0, 1, 2, 3],
            })
            .expect("ingest");
        let resolved = store.resolve(&[id]).expect("resolve");
        let [PromptAttachment::File(file)] = resolved.as_slice() else {
            panic!("expected staged file")
        };
        assert!(file.path.starts_with(directory.path()));
        assert_eq!(file.byte_len, 4);
    }

    #[test]
    fn encoded_image_resolves_to_repository_scoped_file() {
        let directory = tempfile::tempdir().expect("artifact store");
        let store = FrontendArtifactStore::new(directory.path().to_path_buf());
        let id = store
            .ingest(FrontendArtifactInput {
                display_name: "photo.jpg".to_owned(),
                mime_type: Some("image/jpeg".to_owned()),
                kind: FrontendArtifactKind::Image,
                bytes: vec![0xff, 0xd8, 0xff, 0xd9],
            })
            .expect("ingest");
        let resolved = store.resolve(&[id]).expect("resolve");
        let [PromptAttachment::File(file)] = resolved.as_slice() else {
            panic!("expected staged file")
        };
        assert!(file.path.starts_with(directory.path()));
        assert_eq!(file.byte_len, 4);
    }

    #[test]
    fn artifact_ids_cannot_escape_the_store() {
        let directory = tempfile::tempdir().expect("artifact store");
        let store = FrontendArtifactStore::new(directory.path().to_path_buf());
        assert!(matches!(
            store.resolve(&["frontend-artifact-../secret".to_owned()]),
            Err(FrontendArtifactStoreError::InvalidArtifactId(_))
        ));
    }
}
