use std::{
    env,
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

const MAX_OPERATION_BYTES: usize = 128;
const MAX_REPOSITORY_BYTES: usize = 512;
const MAX_RESOURCE_BYTES: usize = 1024;
const MAX_FINGERPRINT_BYTES: usize = 16 * 1024;

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GithubMutationAuditReceipt {
    pub operation: String,
    pub repository: String,
    pub resource: String,
    pub preview_fingerprint: String,
    pub confirmed_at: String,
    pub outcome: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GithubMutationAuditPersistence {
    pub persisted: bool,
    pub receipt_path: String,
}

#[tauri::command]
pub fn runtime_persist_github_mutation_audit(
    receipt: GithubMutationAuditReceipt,
) -> Result<GithubMutationAuditPersistence, String> {
    validate_receipt(&receipt)?;
    let path = audit_path()?;
    persist_receipt(&path, &receipt)?;
    Ok(GithubMutationAuditPersistence {
        persisted: true,
        receipt_path: path.to_string_lossy().into_owned(),
    })
}

fn audit_path() -> Result<PathBuf, String> {
    let root = if let Some(value) = env::var_os("MEDUSA_HOME") {
        PathBuf::from(value)
    } else if cfg!(windows) {
        env::var_os("USERPROFILE")
            .map(PathBuf::from)
            .ok_or_else(|| "cannot resolve Medusa home directory".to_owned())?
            .join(".medusa")
    } else {
        env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or_else(|| "cannot resolve Medusa home directory".to_owned())?
            .join(".medusa")
    };
    Ok(root.join("audit").join("github-mutations.jsonl"))
}

fn persist_receipt(path: &Path, receipt: &GithubMutationAuditReceipt) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| "invalid GitHub audit receipt path".to_owned())?;
    fs::create_dir_all(parent).map_err(|_| "cannot create GitHub audit directory".to_owned())?;
    let mut encoded =
        serde_json::to_vec(receipt).map_err(|_| "cannot encode GitHub audit receipt".to_owned())?;
    encoded.push(b'\n');
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|_| "cannot open GitHub audit receipt ledger".to_owned())?;
    file.write_all(&encoded)
        .and_then(|_| file.sync_all())
        .map_err(|_| "cannot persist GitHub audit receipt".to_owned())
}

fn validate_receipt(receipt: &GithubMutationAuditReceipt) -> Result<(), String> {
    validate_field("operation", &receipt.operation, MAX_OPERATION_BYTES)?;
    validate_field("repository", &receipt.repository, MAX_REPOSITORY_BYTES)?;
    validate_field("resource", &receipt.resource, MAX_RESOURCE_BYTES)?;
    validate_field(
        "preview fingerprint",
        &receipt.preview_fingerprint,
        MAX_FINGERPRINT_BYTES,
    )?;
    validate_field("confirmation timestamp", &receipt.confirmed_at, 256)?;
    validate_field("outcome", &receipt.outcome, 256)?;
    if receipt.repository.contains('@')
        || receipt.repository.to_ascii_lowercase().contains("token=")
        || receipt.preview_fingerprint.contains("ghp_")
        || receipt.preview_fingerprint.contains("github_pat_")
    {
        return Err("GitHub audit receipt contains credential-like material".to_owned());
    }
    Ok(())
}

fn validate_field(name: &str, value: &str, max_bytes: usize) -> Result<(), String> {
    let value = value.trim();
    if value.is_empty() {
        return Err(format!("GitHub audit {name} is required"));
    }
    if value.len() > max_bytes {
        return Err(format!("GitHub audit {name} is too large"));
    }
    if value.contains('\0') || value.contains('\n') || value.contains('\r') {
        return Err(format!(
            "GitHub audit {name} contains invalid control characters"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tempdir;

    fn receipt() -> GithubMutationAuditReceipt {
        GithubMutationAuditReceipt {
            operation: "pullRequestUpdate".to_owned(),
            repository: "octo/repo".to_owned(),
            resource: "pullRequest:42".to_owned(),
            preview_fingerprint: "confirmed".to_owned(),
            confirmed_at: "2026-07-23T00:00:00Z".to_owned(),
            outcome: "updated".to_owned(),
        }
    }

    #[test]
    fn appends_durable_jsonl_receipts() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("audit").join("github.jsonl");
        persist_receipt(&path, &receipt()).expect("persist");
        persist_receipt(&path, &receipt()).expect("persist twice");
        let content = fs::read_to_string(path).expect("read");
        assert_eq!(content.lines().count(), 2);
        assert!(!content.contains("token"));
    }

    #[test]
    fn rejects_credentials_and_multiline_values() {
        let mut value = receipt();
        value.preview_fingerprint = "ghp_super_secret".to_owned();
        assert!(validate_receipt(&value).is_err());
        value.preview_fingerprint = "confirmed".to_owned();
        value.outcome = "updated\nforged".to_owned();
        assert!(validate_receipt(&value).is_err());
    }
}
