//! Production hardening primitives: migrations, recovery, observability, and release manifests.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Component, Path, PathBuf},
    process::Command,
    sync::{Arc, Mutex},
    time::Instant,
};

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use ulid::Ulid;

pub const CURRENT_SCHEMA_VERSION: u32 = 3;

/// One reversible application-state migration.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Migration {
    pub from: u32,
    pub to: u32,
    pub name: String,
}

/// Durable migration receipt with backup provenance.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct MigrationReceipt {
    pub migration_id: String,
    pub from: u32,
    pub to: u32,
    pub backup_directory: PathBuf,
    pub completed_at: String,
    pub before_digest: String,
    pub after_digest: String,
}

/// Versioned state migrator. Every upgrade takes a complete backup first.
pub struct Migrator {
    root: PathBuf,
}

impl Migrator {
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn schema_version(&self) -> MedusaResult<u32> {
        let path = self.root.join("schema-version");
        if !path.exists() {
            return Ok(0);
        }
        fs::read_to_string(path)?
            .trim()
            .parse()
            .map_err(|_| invalid("schema-version is not an integer"))
    }

    pub fn upgrade_to_current(&self) -> MedusaResult<Vec<MigrationReceipt>> {
        fs::create_dir_all(&self.root)?;
        let migrations = [
            Migration { from: 0, to: 1, name: "initialize-layout".into() },
            Migration { from: 1, to: 2, name: "add-observability".into() },
            Migration { from: 2, to: 3, name: "add-release-state".into() },
        ];
        let mut receipts = Vec::new();
        while self.schema_version()? < CURRENT_SCHEMA_VERSION {
            let current = self.schema_version()?;
            let migration = migrations
                .iter()
                .find(|migration| migration.from == current)
                .ok_or_else(|| invalid(format!("no migration from schema {current}")))?;
            receipts.push(self.apply(migration)?);
        }
        Ok(receipts)
    }

    pub fn apply(&self, migration: &Migration) -> MedusaResult<MigrationReceipt> {
        let actual = self.schema_version()?;
        if actual != migration.from || migration.to != migration.from + 1 {
            return Err(invalid(format!(
                "invalid migration {} -> {}; current schema is {actual}",
                migration.from, migration.to
            )));
        }
        let before_digest = directory_digest(&self.root)?;
        let backup_directory = self
            .root
            .join("backups")
            .join(format!("migration-{}", Ulid::new()));
        copy_tree(&self.root, &backup_directory, Some("backups"))?;
        let result = self.apply_contents(migration);
        if let Err(error) = result {
            restore_tree(&backup_directory, &self.root)?;
            return Err(error);
        }
        atomic_write(
            &self.root.join("schema-version"),
            migration.to.to_string().as_bytes(),
        )?;
        let receipt = MigrationReceipt {
            migration_id: format!("mig-{}", Ulid::new()),
            from: migration.from,
            to: migration.to,
            backup_directory,
            completed_at: now()?,
            before_digest,
            after_digest: directory_digest(&self.root)?,
        };
        atomic_json(
            &self
                .root
                .join("migration-history")
                .join(format!("{}.json", receipt.migration_id)),
            &receipt,
        )?;
        Ok(receipt)
    }

    pub fn rollback(&self, receipt: &MigrationReceipt) -> MedusaResult<()> {
        if !receipt.backup_directory.is_dir() {
            return Err(invalid("migration backup is unavailable"));
        }
        restore_tree(&receipt.backup_directory, &self.root)?;
        if directory_digest(&self.root)? != receipt.before_digest {
            return Err(MedusaError::new(
                ErrorCode::ChecksumMismatch,
                ErrorCategory::Persistence,
                "rollback did not restore byte-identical state",
            ));
        }
        Ok(())
    }

    pub fn refuse_unsafe_downgrade(&self, target: u32) -> MedusaResult<()> {
        let current = self.schema_version()?;
        if target < current {
            return Err(MedusaError::new(
                ErrorCode::PolicyDenied,
                ErrorCategory::Policy,
                format!("unsafe downgrade from schema {current} to {target} is refused"),
            ));
        }
        Ok(())
    }

    fn apply_contents(&self, migration: &Migration) -> MedusaResult<()> {
        match migration.to {
            1 => {
                fs::create_dir_all(self.root.join("sessions"))?;
                fs::create_dir_all(self.root.join("memory"))?;
            }
            2 => fs::create_dir_all(self.root.join("observability"))?,
            3 => fs::create_dir_all(self.root.join("release"))?,
            _ => return Err(invalid("unsupported migration target")),
        }
        Ok(())
    }
}

/// Append-only JSONL operational event.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct OperationalEvent {
    pub timestamp: String,
    pub level: String,
    pub component: String,
    pub event: String,
    pub correlation_id: String,
    pub fields: BTreeMap<String, serde_json::Value>,
}

/// Thread-safe metrics and structured event recorder.
#[derive(Clone)]
pub struct Observability {
    root: PathBuf,
    counters: Arc<Mutex<BTreeMap<String, u64>>>,
    durations_ms: Arc<Mutex<BTreeMap<String, Vec<u128>>>>,
}

impl Observability {
    pub fn new(root: impl Into<PathBuf>) -> MedusaResult<Self> {
        let root = root.into();
        fs::create_dir_all(&root)?;
        Ok(Self {
            root,
            counters: Arc::new(Mutex::new(BTreeMap::new())),
            durations_ms: Arc::new(Mutex::new(BTreeMap::new())),
        })
    }

    pub fn increment(&self, name: &str, value: u64) -> MedusaResult<()> {
        validate_metric_name(name)?;
        let mut counters = self.counters.lock().map_err(|_| internal("counter lock poisoned"))?;
        *counters.entry(name.to_owned()).or_default() += value;
        Ok(())
    }

    pub fn record_duration(&self, name: &str, started: Instant) -> MedusaResult<()> {
        validate_metric_name(name)?;
        self.durations_ms
            .lock()
            .map_err(|_| internal("duration lock poisoned"))?
            .entry(name.to_owned())
            .or_default()
            .push(started.elapsed().as_millis());
        Ok(())
    }

    pub fn emit(&self, mut event: OperationalEvent) -> MedusaResult<()> {
        redact_value_map(&mut event.fields);
        let path = self.root.join("events.jsonl");
        let mut line = serde_json::to_vec(&event)?;
        line.push(b'\n');
        append_atomic(&path, &line)
    }

    pub fn snapshot(&self) -> MedusaResult<serde_json::Value> {
        let counters = self.counters.lock().map_err(|_| internal("counter lock poisoned"))?.clone();
        let durations = self
            .durations_ms
            .lock()
            .map_err(|_| internal("duration lock poisoned"))?
            .clone();
        Ok(serde_json::json!({
            "counters": counters,
            "durations_ms": durations,
        }))
    }
}

/// Release artifact entry with checksum and size.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ArtifactEntry {
    pub path: PathBuf,
    pub sha256: String,
    pub size: u64,
}

/// Reproducible release manifest used by package smoke tests.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ReleaseManifest {
    pub version: String,
    pub target: String,
    pub artifacts: Vec<ArtifactEntry>,
    pub generated_at: String,
    pub sbom: PathBuf,
    pub rollback_instructions: PathBuf,
}

pub fn build_release_manifest(
    version: &str,
    target: &str,
    artifact_paths: &[PathBuf],
    sbom: PathBuf,
    rollback_instructions: PathBuf,
) -> MedusaResult<ReleaseManifest> {
    if version.trim().is_empty() || target.trim().is_empty() || artifact_paths.is_empty() {
        return Err(invalid("release manifest requires version, target, and artifacts"));
    }
    let mut artifacts = artifact_paths
        .iter()
        .map(|path| {
            let bytes = fs::read(path)?;
            Ok(ArtifactEntry {
                path: path.clone(),
                sha256: format!("{:x}", Sha256::digest(&bytes)),
                size: bytes.len() as u64,
            })
        })
        .collect::<MedusaResult<Vec<_>>>()?;
    artifacts.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(ReleaseManifest {
        version: version.into(),
        target: target.into(),
        artifacts,
        generated_at: now()?,
        sbom,
        rollback_instructions,
    })
}

/// Validates archive entry paths without extracting them.
pub fn validate_archive_entries<'a>(entries: impl IntoIterator<Item = &'a str>) -> MedusaResult<()> {
    let mut seen = BTreeSet::new();
    for entry in entries {
        let path = Path::new(entry);
        if entry.is_empty()
            || path.is_absolute()
            || path.components().any(|component| {
                matches!(component, Component::ParentDir | Component::RootDir | Component::Prefix(_))
            })
            || !seen.insert(path.to_path_buf())
        {
            return Err(MedusaError::new(
                ErrorCode::PolicyDenied,
                ErrorCategory::Policy,
                format!("unsafe archive entry: {entry}"),
            ));
        }
    }
    Ok(())
}

/// Chaos fixture: repeatedly writes a state file and simulates interrupted temporary files.
pub fn chaos_recovery_cycle(root: &Path, cycles: usize) -> MedusaResult<String> {
    fs::create_dir_all(root)?;
    let state = root.join("state.json");
    for cycle in 0..cycles {
        let temporary = state.with_extension("json.tmp");
        fs::write(&temporary, format!("{{\"cycle\":{cycle}}}"))?;
        if cycle % 3 == 1 {
            fs::remove_file(&temporary)?;
            continue;
        }
        fs::rename(&temporary, &state)?;
        let value: serde_json::Value = serde_json::from_slice(&fs::read(&state)?)?;
        if value.get("cycle").and_then(serde_json::Value::as_u64) != Some(cycle as u64) {
            return Err(internal("chaos recovery observed corrupt state"));
        }
    }
    Ok(format!("chaos-recovery-ok:{cycles}"))
}

/// Executes installation/package smoke checks for a built binary.
pub fn package_smoke(binary: &Path) -> MedusaResult<String> {
    let metadata = fs::metadata(binary)?;
    if !metadata.is_file() || metadata.len() == 0 {
        return Err(invalid("package binary is missing or empty"));
    }
    let output = Command::new(binary).arg("--version").output()?;
    if !output.status.success() {
        return Err(MedusaError::new(
            ErrorCode::ToolExecutionFailed,
            ErrorCategory::Execution,
            String::from_utf8_lossy(&output.stderr),
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn validate_metric_name(name: &str) -> MedusaResult<()> {
    if !name.is_empty()
        && name
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '_' | '.'))
    {
        Ok(())
    } else {
        Err(invalid(format!("invalid metric name: {name}")))
    }
}

fn redact_value_map(fields: &mut BTreeMap<String, serde_json::Value>) {
    for (key, value) in fields {
        let sensitive_key = ["secret", "token", "password", "authorization", "api_key"]
            .iter()
            .any(|needle| key.to_ascii_lowercase().contains(needle));
        if sensitive_key {
            *value = serde_json::Value::String("[REDACTED]".into());
        } else {
            redact_value(value);
        }
    }
}

fn redact_value(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::String(text) => {
            for marker in ["ghp_", "sk-", "Bearer "] {
                if text.contains(marker) {
                    *text = "[REDACTED]".into();
                    break;
                }
            }
        }
        serde_json::Value::Array(values) => values.iter_mut().for_each(redact_value),
        serde_json::Value::Object(values) => values.values_mut().for_each(redact_value),
        _ => {}
    }
}

fn append_atomic(path: &Path, bytes: &[u8]) -> MedusaResult<()> {
    let mut existing = if path.exists() { fs::read(path)? } else { Vec::new() };
    existing.extend_from_slice(bytes);
    atomic_write(path, &existing)
}

fn copy_tree(source: &Path, destination: &Path, skip_name: Option<&str>) -> MedusaResult<()> {
    fs::create_dir_all(destination)?;
    if !source.exists() {
        return Ok(());
    }
    let mut entries = fs::read_dir(source)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
        if skip_name.is_some_and(|name| entry.file_name() == name) {
            continue;
        }
        let source_path = entry.path();
        let destination_path = destination.join(entry.file_name());
        let metadata = fs::symlink_metadata(&source_path)?;
        if metadata.file_type().is_symlink() {
            return Err(MedusaError::new(
                ErrorCode::PolicyDenied,
                ErrorCategory::Policy,
                format!("state tree contains symlink: {}", source_path.display()),
            ));
        }
        if metadata.is_dir() {
            copy_tree(&source_path, &destination_path, None)?;
        } else if metadata.is_file() {
            fs::copy(source_path, destination_path)?;
        }
    }
    Ok(())
}

fn restore_tree(backup: &Path, root: &Path) -> MedusaResult<()> {
    let preserved_backups = root.join("backups");
    for entry in fs::read_dir(root)?.collect::<Result<Vec<_>, _>>()? {
        if entry.path() == preserved_backups {
            continue;
        }
        let metadata = entry.metadata()?;
        if metadata.is_dir() {
            fs::remove_dir_all(entry.path())?;
        } else {
            fs::remove_file(entry.path())?;
        }
    }
    copy_tree(backup, root, None)
}

fn directory_digest(root: &Path) -> MedusaResult<String> {
    if !root.exists() {
        return Ok(format!("{:x}", Sha256::digest([])));
    }
    let mut files = Vec::new();
    collect_files(root, root, &mut files)?;
    files.sort_by(|left, right| left.0.cmp(&right.0));
    let mut hasher = Sha256::new();
    for (relative, bytes) in files {
        hasher.update(relative.to_string_lossy().as_bytes());
        hasher.update([0]);
        hasher.update(bytes);
        hasher.update([0]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn collect_files(root: &Path, current: &Path, files: &mut Vec<(PathBuf, Vec<u8>)>) -> MedusaResult<()> {
    for entry in fs::read_dir(current)?.collect::<Result<Vec<_>, _>>()? {
        let path = entry.path();
        if path.components().any(|component| component.as_os_str() == "backups") {
            continue;
        }
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.file_type().is_symlink() {
            return Err(MedusaError::new(
                ErrorCode::PolicyDenied,
                ErrorCategory::Policy,
                format!("digest tree contains symlink: {}", path.display()),
            ));
        }
        if metadata.is_dir() {
            collect_files(root, &path, files)?;
        } else if metadata.is_file() {
            files.push((path.strip_prefix(root).unwrap_or(&path).to_path_buf(), fs::read(path)?));
        }
    }
    Ok(())
}

fn atomic_json(path: &Path, value: &impl Serialize) -> MedusaResult<()> {
    atomic_write(path, &serde_json::to_vec_pretty(value)?)
}

fn atomic_write(path: &Path, bytes: &[u8]) -> MedusaResult<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension("tmp");
    fs::write(&temporary, bytes)?;
    fs::rename(temporary, path)?;
    Ok(())
}

fn now() -> MedusaResult<String> {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .map_err(|error| internal(error.to_string()))
}

fn invalid(message: impl Into<String>) -> MedusaError {
    MedusaError::new(ErrorCode::InvalidConfiguration, ErrorCategory::Validation, message)
}

fn internal(message: impl Into<String>) -> MedusaError {
    MedusaError::new(ErrorCode::InternalInvariant, ErrorCategory::Internal, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn clean_install_upgrade_and_rollback_are_byte_exact() {
        let directory = tempfile::tempdir().expect("tempdir");
        let root = directory.path().join("state");
        fs::create_dir_all(&root).expect("root");
        fs::write(root.join("legacy.json"), b"legacy-state").expect("legacy");
        let migrator = Migrator::new(&root);
        let receipts = migrator.upgrade_to_current().expect("upgrade");
        assert_eq!(migrator.schema_version().expect("version"), CURRENT_SCHEMA_VERSION);
        assert_eq!(receipts.len(), 3);
        assert!(migrator.refuse_unsafe_downgrade(1).is_err());
        migrator.rollback(receipts.first().expect("first receipt")).expect("rollback");
        assert_eq!(fs::read(root.join("legacy.json")).expect("legacy restored"), b"legacy-state");
    }

    #[test]
    fn chaos_cycle_recovers_without_corruption() {
        let directory = tempfile::tempdir().expect("tempdir");
        assert_eq!(chaos_recovery_cycle(directory.path(), 100).expect("chaos"), "chaos-recovery-ok:100");
    }

    #[test]
    fn operational_events_redact_credentials() {
        let directory = tempfile::tempdir().expect("tempdir");
        let observability = Observability::new(directory.path()).expect("observability");
        observability
            .emit(OperationalEvent {
                timestamp: now().expect("now"),
                level: "info".into(),
                component: "test".into(),
                event: "credential_seen".into(),
                correlation_id: "cor-test".into(),
                fields: BTreeMap::from([
                    ("api_key".into(), serde_json::json!("sk-secret")),
                    ("message".into(), serde_json::json!("Bearer secret")),
                ]),
            })
            .expect("emit");
        let text = fs::read_to_string(directory.path().join("events.jsonl")).expect("events");
        assert!(!text.contains("secret"));
        assert!(text.contains("[REDACTED]"));
    }

    #[test]
    fn release_manifest_is_deterministic_and_checksummed() {
        let directory = tempfile::tempdir().expect("tempdir");
        let binary = directory.path().join("medusa");
        let sbom = directory.path().join("sbom.json");
        let rollback = directory.path().join("ROLLBACK.md");
        fs::write(&binary, b"binary").expect("binary");
        fs::write(&sbom, b"{}").expect("sbom");
        fs::write(&rollback, b"rollback").expect("rollback");
        let first = build_release_manifest(
            "1.0.0",
            "x86_64-unknown-linux-gnu",
            std::slice::from_ref(&binary),
            sbom.clone(),
            rollback.clone(),
        )
        .expect("manifest");
        assert_eq!(first.artifacts[0].sha256, format!("{:x}", Sha256::digest(b"binary")));
        assert_eq!(first.sbom, sbom);
        assert_eq!(first.rollback_instructions, rollback);
    }

    proptest! {
        #[test]
        fn arbitrary_archive_paths_never_escape(segments in proptest::collection::vec("[A-Za-z0-9._-]{0,12}", 0..8)) {
            let entry = segments.join("/");
            let accepted = validate_archive_entries([entry.as_str()]).is_ok();
            if accepted {
                let path = Path::new(&entry);
                prop_assert!(!path.is_absolute());
                prop_assert!(!path.components().any(|component| matches!(component, Component::ParentDir | Component::RootDir | Component::Prefix(_))));
            }
        }

        #[test]
        fn redaction_never_emits_known_secret(secret in "[A-Za-z0-9]{8,32}") {
            let mut value = serde_json::json!(format!("Bearer {secret}"));
            redact_value(&mut value);
            prop_assert!(!value.to_string().contains(&secret));
        }
    }
}
