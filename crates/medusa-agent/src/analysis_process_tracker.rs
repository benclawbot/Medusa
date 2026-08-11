use std::path::{Path, PathBuf};

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use medusa_process_containment::ProcessOwnershipReceipt;
use medusa_process_registry::{
    ProcessId, ProcessRecord, ProcessRegistry, ProcessSpec, ProcessStartMarker, ProcessState,
};
use time::OffsetDateTime;

const REGISTRY_FILE: &str = "analysis-process-registry.json";

/// Crash-durable lifecycle record for one contained analysis helper.
///
/// The registry uses #753's PID + native creation marker identity. The record is written while the
/// child is live, before analysis resumes, so restart/recovery never has to infer ownership from a
/// PID alone.
pub(crate) struct AnalysisProcessTracker {
    path: PathBuf,
    id: ProcessId,
}

impl AnalysisProcessTracker {
    pub(crate) fn started(
        root: &Path,
        program: &str,
        args: &[String],
        receipt: &ProcessOwnershipReceipt,
    ) -> MedusaResult<Self> {
        let path = root.join(REGISTRY_FILE);
        let mut registry = load_or_default(&path)?;
        let now = OffsetDateTime::now_utc();
        let id = ProcessId::parse(format!(
            "analysis-{}-{}",
            receipt.pid,
            now.unix_timestamp_nanos()
        ))
        .map_err(registry_error)?;
        let owner_session = root
            .file_name()
            .map(|value| value.to_string_lossy().into_owned());
        let spec = ProcessSpec {
            program: program.to_owned(),
            args: args.to_vec(),
            working_directory: Some(root.to_path_buf()),
            restartable: false,
        };
        let mut record =
            ProcessRecord::new(id.clone(), spec, now, owner_session).map_err(registry_error)?;
        record
            .mark_running_with_marker(
                receipt.pid,
                Some(
                    ProcessStartMarker::new(
                        receipt.start_marker.platform,
                        receipt.start_marker.value.clone(),
                        receipt.start_marker.boot_id.clone(),
                    )
                    .map_err(registry_error)?,
                ),
                now,
            )
            .map_err(registry_error)?;
        registry.register(record).map_err(registry_error)?;
        registry.save_atomic(&path).map_err(registry_error)?;
        Ok(Self { path, id })
    }

    pub(crate) fn exited(self, exit_code: Option<i32>) -> MedusaResult<()> {
        self.finish(ProcessState::Exited, exit_code, None)
    }

    pub(crate) fn failed(self, failure: &str) -> MedusaResult<()> {
        self.finish(ProcessState::Failed, None, Some(failure))
    }

    fn finish(
        self,
        state: ProcessState,
        exit_code: Option<i32>,
        failure: Option<&str>,
    ) -> MedusaResult<()> {
        let mut registry = ProcessRegistry::load(&self.path).map_err(registry_error)?;
        let record = registry.get_mut(&self.id).map_err(registry_error)?;
        record.exit_code = exit_code;
        record.failure = failure.map(str::to_owned);
        record
            .transition(state, OffsetDateTime::now_utc())
            .map_err(registry_error)?;
        registry.save_atomic(&self.path).map_err(registry_error)
    }
}

fn load_or_default(path: &Path) -> MedusaResult<ProcessRegistry> {
    if path.exists() {
        ProcessRegistry::load(path).map_err(registry_error)
    } else {
        Ok(ProcessRegistry::default())
    }
}

fn registry_error(error: impl std::fmt::Display) -> MedusaError {
    MedusaError::new(
        ErrorCode::InternalInvariant,
        ErrorCategory::Internal,
        format!("analysis process registry failure: {error}"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use medusa_process_containment::OwnedProcessTree;
    use std::process::Command;

    #[test]
    fn persists_native_identity_and_terminal_state() {
        let directory = tempfile::tempdir().expect("tempdir");
        let mut command = if cfg!(windows) {
            let mut command = Command::new("cmd");
            command.args(["/C", "exit", "0"]);
            command
        } else {
            let mut command = Command::new("sh");
            command.args(["-c", "exit 0"]);
            command
        };
        let mut tree = OwnedProcessTree::spawn(&mut command).expect("spawn");
        let tracker = AnalysisProcessTracker::started(
            directory.path(),
            "fixture",
            &[],
            tree.ownership_receipt(),
        )
        .expect("track running process");
        let status = tree.wait().expect("wait");
        tracker.exited(status.code()).expect("track exit");

        let registry =
            ProcessRegistry::load(&directory.path().join(REGISTRY_FILE)).expect("load registry");
        let record = registry.records().next().expect("record");
        assert_eq!(record.state, ProcessState::Exited);
        assert_eq!(record.pid, Some(tree.id()));
        assert!(record.identity.as_ref().is_some_and(|identity| {
            identity
                .start_marker
                .as_ref()
                .is_some_and(|marker| !marker.platform.is_empty() && !marker.value.is_empty())
        }));
    }
}
