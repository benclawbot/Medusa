use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use medusa_core::MedusaResult;
use serde::Serialize;
use time::OffsetDateTime;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum UpdatePhase {
    Check,
    ManifestVerification,
    Download,
    ArtifactVerification,
    Extraction,
    Staging,
    RestartHandoff,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct UpdatePhaseRecord {
    pub recorded_unix_seconds: i64,
    pub phase: UpdatePhase,
    pub elapsed_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retries: Option<u32>,
    pub outcome: String,
}

/// Append-only, path-free updater diagnostics suitable for support bundles.
pub struct UpdateDiagnostics {
    path: PathBuf,
}

impl UpdateDiagnostics {
    #[must_use]
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    #[must_use]
    pub fn for_repository(repository: &Path) -> Self {
        Self::new(repository.join(".medusa/update-diagnostics.jsonl"))
    }

    #[must_use]
    pub fn phase(&self, phase: UpdatePhase) -> PhaseTimer<'_> {
        PhaseTimer {
            diagnostics: self,
            phase,
            started: Instant::now(),
        }
    }

    fn append(&self, record: &UpdatePhaseRecord) -> MedusaResult<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut output = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        serde_json::to_writer(&mut output, record).map_err(std::io::Error::other)?;
        output.write_all(b"\n")?;
        output.sync_data()?;
        Ok(())
    }
}

pub struct PhaseTimer<'a> {
    diagnostics: &'a UpdateDiagnostics,
    phase: UpdatePhase,
    started: Instant,
}

impl PhaseTimer<'_> {
    pub fn finish(
        self,
        outcome: impl Into<String>,
        bytes: Option<u64>,
        retries: Option<u32>,
    ) -> MedusaResult<Duration> {
        let elapsed = self.started.elapsed();
        self.diagnostics.append(&UpdatePhaseRecord {
            recorded_unix_seconds: OffsetDateTime::now_utc().unix_timestamp(),
            phase: self.phase,
            elapsed_ms: elapsed.as_millis().try_into().unwrap_or(u64::MAX),
            bytes,
            retries,
            outcome: outcome.into(),
        })?;
        Ok(elapsed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagnostics_do_not_record_urls_or_paths() {
        let directory = tempfile::tempdir().expect("tempdir");
        let diagnostics = UpdateDiagnostics::new(directory.path().join("update.jsonl"));
        diagnostics
            .phase(UpdatePhase::Download)
            .finish("verified", Some(42), Some(1))
            .expect("record");
        let content = fs::read_to_string(directory.path().join("update.jsonl")).expect("record");
        assert!(content.contains("download"));
        assert!(content.contains("42"));
        assert!(!content.contains("http"));
        assert!(!content.contains(directory.path().to_string_lossy().as_ref()));
    }
}
