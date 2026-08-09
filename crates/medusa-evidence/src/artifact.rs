use std::{
    fs,
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use ulid::Ulid;

use crate::{
    DEFAULT_PAGE_SIZE, EvidenceError, MAX_SEARCH_HITS, Result, SCHEMA_VERSION, fingerprint,
    hash_bytes, write_atomic, write_json_atomic,
};

#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct ArtifactId(pub String);

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ArtifactMetadata {
    pub schema_version: u16,
    pub id: ArtifactId,
    pub media_type: String,
    pub byte_len: u64,
    pub sha256: String,
    pub producer: String,
    pub created_at: OffsetDateTime,
    pub binary: bool,
    pub page_size: u64,
    pub page_count: u64,
    pub fingerprint: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ArtifactReadReceipt {
    pub schema_version: u16,
    pub id: String,
    pub artifact_id: ArtifactId,
    pub offset: u64,
    pub length: u64,
    pub content_hash: String,
    pub reader: String,
    pub read_at: OffsetDateTime,
    pub fingerprint: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ArtifactSearchHit {
    pub offset: u64,
    pub length: u64,
    pub preview: String,
}

#[derive(Clone, Debug)]
pub struct ArtifactStore {
    root: PathBuf,
}

impl ArtifactStore {
    pub fn open(root: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
        for child in ["objects", "metadata", "reads"] {
            fs::create_dir_all(root.join(child))?;
        }
        Ok(Self { root })
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn put_bytes(
        &self,
        media_type: impl Into<String>,
        producer: impl Into<String>,
        bytes: &[u8],
    ) -> Result<ArtifactMetadata> {
        let media_type = media_type.into();
        let producer = producer.into();
        if media_type.trim().is_empty() || producer.trim().is_empty() {
            return Err(EvidenceError::Validation(
                "artifact metadata requires media type and producer".to_owned(),
            ));
        }
        let sha256 = hash_bytes(bytes);
        let id = ArtifactId(format!("artifact-{sha256}"));
        let mut metadata = ArtifactMetadata {
            schema_version: SCHEMA_VERSION,
            id: id.clone(),
            media_type,
            byte_len: bytes.len() as u64,
            sha256: sha256.clone(),
            producer,
            created_at: OffsetDateTime::now_utc(),
            binary: is_binary(bytes),
            page_size: DEFAULT_PAGE_SIZE,
            page_count: if bytes.is_empty() {
                0
            } else {
                (bytes.len() as u64).div_ceil(DEFAULT_PAGE_SIZE)
            },
            fingerprint: String::new(),
        };
        metadata.fingerprint = metadata_fingerprint(&metadata);
        let object_path = self.object_path(&id);
        if object_path.is_file() {
            let existing = fs::read(&object_path)?;
            if existing != bytes {
                if hash_bytes(&existing) == sha256 {
                    return Err(EvidenceError::Validation(
                        "content-addressed artifact collision".to_owned(),
                    ));
                }
                write_atomic(&object_path, bytes)?;
            }
        } else {
            write_atomic(&object_path, bytes)?;
        }
        let metadata_path = self.metadata_path(&id);
        if metadata_path.is_file()
            && let Ok(existing) = self.metadata(&id)
        {
            return Ok(existing);
        }
        write_json_atomic(&metadata_path, &metadata)?;
        Ok(metadata)
    }

    pub fn metadata(&self, id: &ArtifactId) -> Result<ArtifactMetadata> {
        if !valid_artifact_id(id) {
            return Err(EvidenceError::Validation(
                "artifact id is incomplete or corrupted".to_owned(),
            ));
        }
        let path = self.metadata_path(id);
        if !path.is_file() {
            return Err(EvidenceError::NotFound(id.0.clone()));
        }
        let metadata: ArtifactMetadata = serde_json::from_slice(&fs::read(path)?)?;
        validate_metadata(&metadata)?;
        let bytes = fs::read(self.object_path(id))?;
        if bytes.len() as u64 != metadata.byte_len || hash_bytes(&bytes) != metadata.sha256 {
            return Err(EvidenceError::Validation(format!(
                "artifact {} bytes do not match metadata",
                id.0
            )));
        }
        self.repair_persisted_read_receipts(id, &bytes)?;
        Ok(metadata)
    }

    fn repair_persisted_read_receipts(&self, id: &ArtifactId, bytes: &[u8]) -> Result<()> {
        let receipt_path = self.root.join("verification-receipt.json");
        if !receipt_path.is_file() {
            return Ok(());
        }
        let Ok(value) = serde_json::from_slice::<serde_json::Value>(&fs::read(receipt_path)?)
        else {
            return Ok(());
        };
        let Some(reads) = value
            .get("evidence")
            .and_then(|evidence| evidence.get("reads"))
            .and_then(serde_json::Value::as_array)
        else {
            return Ok(());
        };
        for value in reads {
            let Ok(expected) = serde_json::from_value::<ArtifactReadReceipt>(value.clone()) else {
                continue;
            };
            if expected.artifact_id != *id || validate_read(&expected).is_err() {
                continue;
            }
            let end = expected.offset.saturating_add(expected.length);
            if end > bytes.len() as u64
                || hash_bytes(&bytes[expected.offset as usize..end as usize])
                    != expected.content_hash
            {
                return Err(EvidenceError::Validation(format!(
                    "artifact read receipt {} does not match artifact bytes",
                    expected.id
                )));
            }
            if !self
                .load_read_receipt(&expected.id)
                .is_ok_and(|actual| actual == expected)
            {
                write_json_atomic(
                    &self
                        .root
                        .join("reads")
                        .join(format!("{}.json", expected.id)),
                    &expected,
                )?;
            }
        }
        Ok(())
    }

    pub fn read_range(
        &self,
        id: &ArtifactId,
        offset: u64,
        length: u64,
        reader: impl Into<String>,
    ) -> Result<(Vec<u8>, ArtifactReadReceipt)> {
        let reader = reader.into();
        if reader.trim().is_empty() || length == 0 {
            return Err(EvidenceError::Validation(
                "artifact range reads require reader and non-zero length".to_owned(),
            ));
        }
        let metadata = self.metadata(id)?;
        if offset >= metadata.byte_len || offset.saturating_add(length) > metadata.byte_len {
            return Err(EvidenceError::Validation(format!(
                "artifact range {offset}..{} exceeds {} bytes",
                offset.saturating_add(length),
                metadata.byte_len
            )));
        }
        let mut file = fs::File::open(self.object_path(id))?;
        file.seek(SeekFrom::Start(offset))?;
        let mut bytes = vec![0; length as usize];
        file.read_exact(&mut bytes)?;
        let mut receipt = ArtifactReadReceipt {
            schema_version: SCHEMA_VERSION,
            id: format!("read-{}", Ulid::new()),
            artifact_id: id.clone(),
            offset,
            length,
            content_hash: hash_bytes(&bytes),
            reader,
            read_at: OffsetDateTime::now_utc(),
            fingerprint: String::new(),
        };
        receipt.fingerprint = read_fingerprint(&receipt);
        write_json_atomic(
            &self.root.join("reads").join(format!("{}.json", receipt.id)),
            &receipt,
        )?;
        Ok((bytes, receipt))
    }

    pub fn read_page(
        &self,
        id: &ArtifactId,
        page: u64,
        reader: impl Into<String>,
    ) -> Result<(Vec<u8>, ArtifactReadReceipt)> {
        let metadata = self.metadata(id)?;
        if page >= metadata.page_count {
            return Err(EvidenceError::Validation(format!(
                "artifact page {page} is outside {} pages",
                metadata.page_count
            )));
        }
        let offset = page * metadata.page_size;
        self.read_range(
            id,
            offset,
            metadata.page_size.min(metadata.byte_len - offset),
            reader,
        )
    }

    pub fn search_text(
        &self,
        id: &ArtifactId,
        query: &str,
        reader: impl Into<String>,
    ) -> Result<(Vec<ArtifactSearchHit>, ArtifactReadReceipt)> {
        if query.trim().is_empty() {
            return Err(EvidenceError::Validation(
                "artifact search query cannot be empty".to_owned(),
            ));
        }
        let metadata = self.metadata(id)?;
        if metadata.binary || metadata.byte_len == 0 {
            return Err(EvidenceError::Validation(
                "binary or empty artifact cannot be text-searched".to_owned(),
            ));
        }
        let (bytes, receipt) = self.read_range(id, 0, metadata.byte_len, reader)?;
        let text = std::str::from_utf8(&bytes)
            .map_err(|_| EvidenceError::Validation("artifact is not valid UTF-8".to_owned()))?;
        let hits = text
            .match_indices(query)
            .take(MAX_SEARCH_HITS)
            .map(|(offset, _)| {
                let start = offset.saturating_sub(48);
                let end = (offset + query.len() + 48).min(text.len());
                ArtifactSearchHit {
                    offset: offset as u64,
                    length: query.len() as u64,
                    preview: text
                        .get(start..end)
                        .unwrap_or(query)
                        .replace(['\n', '\r'], " "),
                }
            })
            .collect();
        Ok((hits, receipt))
    }

    pub fn load_read_receipt(&self, id: &str) -> Result<ArtifactReadReceipt> {
        if !valid_read_id(id) {
            return Err(EvidenceError::Validation(
                "artifact read receipt id is incomplete or corrupted".to_owned(),
            ));
        }
        let path = self.root.join("reads").join(format!("{id}.json"));
        if !path.is_file() {
            return Err(EvidenceError::NotFound(id.to_owned()));
        }
        let receipt: ArtifactReadReceipt = serde_json::from_slice(&fs::read(path)?)?;
        validate_read(&receipt)?;
        Ok(receipt)
    }

    fn object_path(&self, id: &ArtifactId) -> PathBuf {
        self.root.join("objects").join(format!("{}.bin", id.0))
    }

    fn metadata_path(&self, id: &ArtifactId) -> PathBuf {
        self.root.join("metadata").join(format!("{}.json", id.0))
    }
}

pub(crate) fn validate_metadata(metadata: &ArtifactMetadata) -> Result<()> {
    if metadata.schema_version != SCHEMA_VERSION
        || !valid_artifact_id(&metadata.id)
        || metadata.id.0 != format!("artifact-{}", metadata.sha256)
        || metadata.page_size == 0
        || metadata.page_count
            != if metadata.byte_len == 0 {
                0
            } else {
                metadata.byte_len.div_ceil(metadata.page_size)
            }
        || metadata.fingerprint != metadata_fingerprint(metadata)
    {
        return Err(EvidenceError::Validation(
            "artifact metadata is incomplete or corrupted".to_owned(),
        ));
    }
    Ok(())
}

fn valid_artifact_id(id: &ArtifactId) -> bool {
    id.0
        .strip_prefix("artifact-")
        .is_some_and(|hash| {
            hash.len() == 64
                && hash
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
}

pub(crate) fn validate_read(receipt: &ArtifactReadReceipt) -> Result<()> {
    if receipt.schema_version != SCHEMA_VERSION
        || !valid_read_id(&receipt.id)
        || receipt.reader.trim().is_empty()
        || receipt.length == 0
        || receipt.fingerprint != read_fingerprint(receipt)
    {
        return Err(EvidenceError::Validation(
            "artifact read receipt is incomplete or corrupted".to_owned(),
        ));
    }
    Ok(())
}

fn valid_read_id(id: &str) -> bool {
    id.strip_prefix("read-")
        .is_some_and(|value| value.parse::<Ulid>().is_ok())
}

fn metadata_fingerprint(metadata: &ArtifactMetadata) -> String {
    fingerprint(&(
        metadata.schema_version,
        &metadata.id,
        &metadata.media_type,
        metadata.byte_len,
        &metadata.sha256,
        &metadata.producer,
        metadata.created_at,
        metadata.binary,
        metadata.page_size,
        metadata.page_count,
    ))
}

fn read_fingerprint(receipt: &ArtifactReadReceipt) -> String {
    fingerprint(&(
        receipt.schema_version,
        &receipt.id,
        &receipt.artifact_id,
        receipt.offset,
        receipt.length,
        &receipt.content_hash,
        &receipt.reader,
        receipt.read_at,
    ))
}

fn is_binary(bytes: &[u8]) -> bool {
    bytes.iter().take(8 * 1024).any(|byte| *byte == 0) || std::str::from_utf8(bytes).is_err()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn middle_range_is_stable_and_read_is_durable() {
        let directory = tempfile::tempdir().expect("tempdir");
        let store = ArtifactStore::open(directory.path()).expect("store");
        let bytes = (0..100_000)
            .map(|index| b'a' + (index % 26) as u8)
            .collect::<Vec<_>>();
        let artifact = store
            .put_bytes("text/plain", "test", &bytes)
            .expect("artifact");
        let (middle, receipt) = store
            .read_range(&artifact.id, 40_000, 128, "reviewer")
            .expect("range");
        assert_eq!(middle, bytes[40_000..40_128]);
        assert_eq!(store.load_read_receipt(&receipt.id).unwrap(), receipt);
    }

    #[test]
    fn artifact_metadata_rejects_path_traversal_even_with_valid_fingerprint() {
        let directory = tempfile::tempdir().expect("tempdir");
        let store = ArtifactStore::open(directory.path()).expect("store");
        let mut metadata = store
            .put_bytes("text/plain", "test", b"authoritative evidence")
            .expect("artifact");
        metadata.sha256 = "../../../../../tmp/victim".to_owned();
        metadata.id = ArtifactId(format!("artifact-{}", metadata.sha256));
        metadata.fingerprint = metadata_fingerprint(&metadata);

        assert!(validate_metadata(&metadata).is_err());
        assert!(store.metadata(&metadata.id).is_err());
    }

    #[test]
    fn read_receipt_id_rejects_path_traversal_even_with_valid_fingerprint() {
        let directory = tempfile::tempdir().expect("tempdir");
        let store = ArtifactStore::open(directory.path()).expect("store");
        let bytes = b"authoritative evidence";
        let artifact = store
            .put_bytes("text/plain", "test", bytes)
            .expect("artifact");
        let (_, receipt) = store
            .read_range(&artifact.id, 0, bytes.len() as u64, "reviewer")
            .expect("read");
        let mut malicious = receipt;
        malicious.id = "../../victim".to_owned();
        malicious.fingerprint = read_fingerprint(&malicious);

        assert!(validate_read(&malicious).is_err());
    }

    #[test]
    fn put_bytes_repairs_corrupted_cached_object_and_metadata() {
        let directory = tempfile::tempdir().expect("tempdir");
        let store = ArtifactStore::open(directory.path()).expect("store");
        let bytes = b"authoritative evidence";
        let artifact = store
            .put_bytes("text/plain", "test", bytes)
            .expect("artifact");
        let object_path = store.object_path(&artifact.id);
        let metadata_path = store.metadata_path(&artifact.id);

        fs::write(&object_path, b"corrupted").expect("corrupt object");
        let repaired_object = store
            .put_bytes("text/plain", "test", bytes)
            .expect("repair object");
        assert_eq!(fs::read(&object_path).expect("object"), bytes);
        assert_eq!(store.metadata(&artifact.id).unwrap(), repaired_object);

        fs::write(&metadata_path, b"{broken-metadata").expect("corrupt metadata");
        let repaired_metadata = store
            .put_bytes("text/plain", "test", bytes)
            .expect("repair metadata");
        assert_eq!(store.metadata(&artifact.id).unwrap(), repaired_metadata);
    }

    #[test]
    fn metadata_repairs_persisted_read_receipt_from_authoritative_receipt() {
        let directory = tempfile::tempdir().expect("tempdir");
        let store = ArtifactStore::open(directory.path()).expect("store");
        let bytes = b"authoritative evidence";
        let artifact = store
            .put_bytes("text/plain", "test", bytes)
            .expect("artifact");
        let (_, expected) = store
            .read_range(&artifact.id, 0, bytes.len() as u64, "reviewer")
            .expect("read");
        fs::write(
            store.root().join("verification-receipt.json"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "evidence": { "reads": [expected.clone()] }
            }))
            .expect("receipt json"),
        )
        .expect("persist receipt");
        fs::write(
            store
                .root()
                .join("reads")
                .join(format!("{}.json", expected.id)),
            b"{broken-read-receipt",
        )
        .expect("corrupt read");

        store.metadata(&artifact.id).expect("repair reads");
        assert_eq!(store.load_read_receipt(&expected.id).unwrap(), expected);
    }
}
