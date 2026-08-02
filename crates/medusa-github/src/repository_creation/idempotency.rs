use std::{
    fs::{self, OpenOptions},
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    thread,
    time::{Duration, SystemTime},
};

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use ulid::Ulid;

use super::*;

const MAX_LEDGER_BYTES: u64 = 32 * 1_048_576;
const LOCK_RETRIES: usize = 200;
const LOCK_DELAY: Duration = Duration::from_millis(10);
const STALE_LOCK_AGE: Duration = Duration::from_secs(300);

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GitHubOperationLifecycleStatus {
    Requested,
    Authorized,
    Dispatched,
    Accepted,
    Completed,
    Persisted,
    Uncertain,
    Reconciled,
    Failed,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GitHubReconciliationStatus {
    NotRequired,
    Pending,
    Confirmed,
    Absent,
    Inconclusive,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitHubOperationReceiptV2 {
    pub schema_version: u16,
    pub attempt_id: String,
    pub request_digest: String,
    pub idempotency_key_hash: Option<String>,
    pub lifecycle_status: GitHubOperationLifecycleStatus,
    pub reconciliation_status: GitHubReconciliationStatus,
    pub repository: String,
    pub hostname: String,
    pub operation_kind: String,
    pub risk: GitHubOperationRisk,
    pub backend: GitHubExecutionBackend,
    pub transport: String,
    pub mutated: bool,
    pub resource_identity: Option<String>,
    pub resource_url: Option<String>,
    pub http: GitHubHttpMetadata,
    pub retry_count: u8,
    pub retry_reason: Option<String>,
    pub api_version: String,
    pub canonical: Value,
    pub payload: Value,
    pub artifact: Option<GitHubArtifactReceipt>,
    pub truncated: bool,
    pub redacted: bool,
    pub audit_persisted: bool,
    pub replayed: bool,
}

impl GitHubOperationReceiptV2 {
    pub fn from_backend(
        document: &GitHubOperationDocument,
        prepared: &PreparedGitHubOperation,
        attempt_id: String,
        request_digest: String,
        backend: GitHubBackendExecution,
    ) -> Self {
        Self {
            schema_version: GITHUB_OPERATION_SCHEMA_VERSION,
            attempt_id,
            request_digest,
            idempotency_key_hash: document.idempotency_key_hash(),
            lifecycle_status: GitHubOperationLifecycleStatus::Completed,
            reconciliation_status: if prepared.reconciliation.is_some() {
                GitHubReconciliationStatus::Pending
            } else {
                GitHubReconciliationStatus::NotRequired
            },
            repository: document.repository.clone(),
            hostname: document.hostname.clone(),
            operation_kind: prepared.operation_kind.clone(),
            risk: prepared.risk,
            backend: document.backend,
            transport: backend.transport,
            mutated: backend.mutated,
            resource_identity: backend.resource_identity,
            resource_url: backend.resource_url,
            http: backend.http,
            retry_count: backend.retry_count,
            retry_reason: backend.retry_reason,
            api_version: document.api_version.clone(),
            canonical: backend.canonical,
            payload: backend.payload,
            artifact: backend.artifact,
            truncated: backend.truncated,
            redacted: backend.redacted,
            audit_persisted: false,
            replayed: false,
        }
    }

    #[must_use]
    pub fn replayed(mut self) -> Self {
        self.replayed = true;
        self
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitHubOperationLifecycleEvent {
    pub attempt_id: String,
    pub request_digest: String,
    pub idempotency_key_hash: Option<String>,
    pub repository: String,
    pub hostname: String,
    pub operation_kind: String,
    pub status: GitHubOperationLifecycleStatus,
    pub detail: Option<String>,
    pub recorded_at_unix: i64,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "recordType", rename_all = "snake_case")]
enum LedgerRecord {
    Lifecycle { event: GitHubOperationLifecycleEvent },
    Receipt { receipt: GitHubOperationReceiptV2 },
}

#[derive(Clone, Debug)]
pub struct FileGitHubOperationLedger {
    path: PathBuf,
    lock_path: PathBuf,
}

#[derive(Clone, Debug, PartialEq)]
pub enum IdempotencyLookup {
    Miss,
    Replay(GitHubOperationReceiptV2),
    ResumeUncertain,
}

impl FileGitHubOperationLedger {
    pub fn at(root: &Path) -> MedusaResult<Self> {
        let directory = root.join("state");
        fs::create_dir_all(&directory).map_err(persistence_error)?;
        let path = directory.join("github-operations-ledger.jsonl");
        let lock_path = directory.join("github-operations-ledger.lock");
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(persistence_error)?;
        file.sync_all().map_err(persistence_error)?;
        Ok(Self { path, lock_path })
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn lookup(
        &self,
        key_hash: Option<&str>,
        request_digest: &str,
    ) -> MedusaResult<IdempotencyLookup> {
        let Some(key_hash) = key_hash else {
            return Ok(IdempotencyLookup::Miss);
        };
        let _guard = self.acquire_lock()?;
        if !self.path.exists() {
            return Ok(IdempotencyLookup::Miss);
        }
        let metadata = fs::metadata(&self.path).map_err(persistence_error)?;
        if metadata.len() > MAX_LEDGER_BYTES {
            return Err(persistence_failure(
                "GitHub idempotency ledger exceeded its bounded size; rotate or archive it",
            ));
        }
        let file = fs::File::open(&self.path).map_err(persistence_error)?;
        let mut matched_digest = false;
        let mut uncertain = false;
        let mut receipt = None;
        for line in BufReader::new(file).lines() {
            let line = line.map_err(persistence_error)?;
            if line.trim().is_empty() {
                continue;
            }
            let record: LedgerRecord = serde_json::from_str(&line).map_err(|error| {
                persistence_failure(format!("decode GitHub idempotency ledger: {error}"))
            })?;
            match record {
                LedgerRecord::Lifecycle { event }
                    if event.idempotency_key_hash.as_deref() == Some(key_hash) =>
                {
                    if event.request_digest != request_digest {
                        return Err(policy_error(
                            "GitHub idempotency key was already used for a different request digest",
                        ));
                    }
                    matched_digest = true;
                    uncertain = matches!(
                        event.status,
                        GitHubOperationLifecycleStatus::Dispatched
                            | GitHubOperationLifecycleStatus::Accepted
                            | GitHubOperationLifecycleStatus::Uncertain
                    );
                    if matches!(
                        event.status,
                        GitHubOperationLifecycleStatus::Completed
                            | GitHubOperationLifecycleStatus::Persisted
                            | GitHubOperationLifecycleStatus::Reconciled
                            | GitHubOperationLifecycleStatus::Failed
                    ) {
                        uncertain = false;
                    }
                }
                LedgerRecord::Receipt { receipt: candidate }
                    if candidate.idempotency_key_hash.as_deref() == Some(key_hash) =>
                {
                    if candidate.request_digest != request_digest {
                        return Err(policy_error(
                            "GitHub idempotency key was already used for a different request digest",
                        ));
                    }
                    matched_digest = true;
                    uncertain = false;
                    if matches!(
                        candidate.lifecycle_status,
                        GitHubOperationLifecycleStatus::Completed
                            | GitHubOperationLifecycleStatus::Persisted
                            | GitHubOperationLifecycleStatus::Reconciled
                    ) {
                        receipt = Some(candidate);
                    }
                }
                _ => {}
            }
        }
        if let Some(receipt) = receipt {
            Ok(IdempotencyLookup::Replay(receipt.replayed()))
        } else if matched_digest && uncertain {
            Ok(IdempotencyLookup::ResumeUncertain)
        } else {
            Ok(IdempotencyLookup::Miss)
        }
    }

    pub fn append_lifecycle(
        &self,
        document: &GitHubOperationDocument,
        prepared: &PreparedGitHubOperation,
        attempt_id: &str,
        request_digest: &str,
        status: GitHubOperationLifecycleStatus,
        detail: Option<String>,
    ) -> MedusaResult<()> {
        let event = GitHubOperationLifecycleEvent {
            attempt_id: attempt_id.into(),
            request_digest: request_digest.into(),
            idempotency_key_hash: document.idempotency_key_hash(),
            repository: document.repository.clone(),
            hostname: document.hostname.clone(),
            operation_kind: prepared.operation_kind.clone(),
            status,
            detail: detail.map(sanitize_detail),
            recorded_at_unix: now(),
        };
        self.append(&LedgerRecord::Lifecycle { event })
    }

    pub fn append_receipt(&self, receipt: &GitHubOperationReceiptV2) -> MedusaResult<()> {
        self.append(&LedgerRecord::Receipt {
            receipt: receipt.clone(),
        })
    }

    fn append(&self, record: &LedgerRecord) -> MedusaResult<()> {
        let _guard = self.acquire_lock()?;
        if self.path.exists()
            && fs::metadata(&self.path).map_err(persistence_error)?.len() > MAX_LEDGER_BYTES
        {
            return Err(persistence_failure(
                "GitHub idempotency ledger exceeded its bounded size; rotate or archive it",
            ));
        }
        let mut encoded = serde_json::to_vec(record).map_err(|error| {
            persistence_failure(format!("encode GitHub idempotency record: {error}"))
        })?;
        encoded.push(b'\n');
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .map_err(persistence_error)?;
        file.write_all(&encoded).map_err(persistence_error)?;
        file.sync_all().map_err(persistence_error)?;
        sync_parent(&self.path);
        Ok(())
    }

    fn acquire_lock(&self) -> MedusaResult<LedgerLock> {
        for _ in 0..LOCK_RETRIES {
            match OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&self.lock_path)
            {
                Ok(mut file) => {
                    writeln!(file, "{}", std::process::id()).map_err(persistence_error)?;
                    file.sync_all().map_err(persistence_error)?;
                    return Ok(LedgerLock {
                        path: self.lock_path.clone(),
                    });
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    if stale_lock(&self.lock_path) {
                        let _ = fs::remove_file(&self.lock_path);
                        continue;
                    }
                    thread::sleep(LOCK_DELAY);
                }
                Err(error) => return Err(persistence_error(error)),
            }
        }
        Err(persistence_failure(
            "timed out acquiring GitHub idempotency ledger lock",
        ))
    }
}

struct LedgerLock {
    path: PathBuf,
}

impl Drop for LedgerLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

#[must_use]
pub fn new_github_attempt_id() -> String {
    format!("gha-{}", Ulid::new())
}

fn stale_lock(path: &Path) -> bool {
    fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|modified| SystemTime::now().duration_since(modified).ok())
        .is_some_and(|age| age >= STALE_LOCK_AGE)
}

fn sanitize_detail(value: String) -> String {
    let mut sanitized = value;
    for prefix in [
        "ghp_",
        "gho_",
        "ghu_",
        "ghs_",
        "ghr_",
        "github_pat_",
        "Bearer ",
        "bearer ",
    ] {
        while let Some(index) = sanitized.find(prefix) {
            let end = sanitized[index..]
                .find(char::is_whitespace)
                .map(|offset| index + offset)
                .unwrap_or(sanitized.len());
            sanitized.replace_range(index..end, "[REDACTED]");
        }
    }
    sanitized.chars().take(2048).collect()
}

fn now() -> i64 {
    time::OffsetDateTime::now_utc().unix_timestamp()
}

fn sync_parent(path: &Path) {
    #[cfg(unix)]
    if let Some(parent) = path.parent()
        && let Ok(directory) = fs::File::open(parent)
    {
        let _ = directory.sync_all();
    }
    #[cfg(not(unix))]
    let _ = path;
}

fn persistence_error(error: std::io::Error) -> MedusaError {
    MedusaError::new(
        ErrorCode::PersistenceFailed,
        ErrorCategory::Persistence,
        error.to_string(),
    )
}

fn persistence_failure(message: impl Into<String>) -> MedusaError {
    MedusaError::new(
        ErrorCode::PersistenceFailed,
        ErrorCategory::Persistence,
        message,
    )
}

fn policy_error(message: impl Into<String>) -> MedusaError {
    MedusaError::new(ErrorCode::PolicyDenied, ErrorCategory::Policy, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn document() -> GitHubOperationDocument {
        GitHubOperationDocument {
            schema_version: GITHUB_OPERATION_SCHEMA_VERSION,
            repository: "acme/project".into(),
            hostname: "github.com".into(),
            api_base_url: None,
            oauth_base_url: None,
            api_version: DEFAULT_GITHUB_API_VERSION.into(),
            backend: GitHubExecutionBackend::DirectOauth,
            idempotency_key: Some("create-issue-7".into()),
            max_response_bytes: 1024,
            operation: GitHubTypedOperation::Issues(IssuesOperation::Create {
                title: "title".into(),
                body: "body".into(),
                labels: vec![],
                assignees: vec![],
                milestone: None,
            }),
        }
    }

    fn receipt(document: &GitHubOperationDocument, prepared: &PreparedGitHubOperation) -> GitHubOperationReceiptV2 {
        GitHubOperationReceiptV2::from_backend(
            document,
            prepared,
            new_github_attempt_id(),
            document.request_digest().expect("digest"),
            GitHubBackendExecution {
                transport: "test".into(),
                mutated: true,
                resource_identity: Some("7".into()),
                resource_url: None,
                canonical: Value::Object(Default::default()),
                payload: Value::Object(Default::default()),
                truncated: false,
                redacted: false,
                http: GitHubHttpMetadata::default(),
                retry_count: 0,
                retry_reason: None,
                artifact: None,
            },
        )
    }

    #[test]
    fn exact_key_and_digest_replays_prior_receipt() {
        let root = tempfile::tempdir().expect("root");
        let ledger = FileGitHubOperationLedger::at(root.path()).expect("ledger");
        let document = document();
        let prepared = document.prepare().expect("prepare");
        let mut receipt = receipt(&document, &prepared);
        receipt.lifecycle_status = GitHubOperationLifecycleStatus::Persisted;
        ledger.append_receipt(&receipt).expect("append");
        assert!(matches!(
            ledger.lookup(document.idempotency_key_hash().as_deref(), &document.request_digest().expect("digest")).expect("lookup"),
            IdempotencyLookup::Replay(value) if value.replayed
        ));
    }

    #[test]
    fn key_reuse_with_different_digest_is_rejected() {
        let root = tempfile::tempdir().expect("root");
        let ledger = FileGitHubOperationLedger::at(root.path()).expect("ledger");
        let document = document();
        let prepared = document.prepare().expect("prepare");
        let receipt = receipt(&document, &prepared);
        ledger.append_receipt(&receipt).expect("append");
        assert!(ledger.lookup(document.idempotency_key_hash().as_deref(), "different").is_err());
    }

    #[test]
    fn uncertain_dispatch_can_be_resumed_for_reconciliation() {
        let root = tempfile::tempdir().expect("root");
        let ledger = FileGitHubOperationLedger::at(root.path()).expect("ledger");
        let document = document();
        let prepared = document.prepare().expect("prepare");
        let digest = document.request_digest().expect("digest");
        ledger.append_lifecycle(&document, &prepared, "attempt", &digest, GitHubOperationLifecycleStatus::Uncertain, None).expect("append");
        assert_eq!(ledger.lookup(document.idempotency_key_hash().as_deref(), &digest).expect("lookup"), IdempotencyLookup::ResumeUncertain);
    }

    #[test]
    fn lifecycle_detail_redacts_tokens() {
        assert!(!sanitize_detail("failed ghu_secret".into()).contains("ghu_secret"));
    }
}
