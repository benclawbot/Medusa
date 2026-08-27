//! Persistent, subordinate analysis workspace for context-as-data computation.
//!
//! This is deliberately not a second agent runtime. It owns no provider client, repository
//! mutation authority, ambient network access, or child-agent scheduler. Large inputs are copied
//! into immutable content-addressed artifacts, bounded deterministic reductions run over those
//! artifacts, and recursive delegation is expressed as typed requests to the existing team
//! authority.

use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::atomic::Ordering,
    thread,
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{RuntimeController, RuntimeError, TeamSnapshot};

const FORMAT_VERSION: u16 = 1;
const MAX_IMPORTED_ARTIFACT_BYTES: u64 = 64 * 1024 * 1024;
const MAX_VARIABLES: usize = 256;
const MAX_TEXT_VALUE_BYTES: usize = 32 * 1024;
const MAX_RESULT_TEXT_BYTES: usize = 16 * 1024;
const MAX_SAMPLE_LINES: usize = 128;
const MAX_DELEGATION_MESSAGE_BYTES: usize = 4 * 1024;
const DELEGATION_AWAIT_POLL_INTERVAL: Duration = Duration::from_millis(25);
const DELEGATION_AWAIT_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AnalysisValue {
    Null,
    Bool(bool),
    Integer(i64),
    Text(String),
    StringList(Vec<String>),
    Artifact(AnalysisArtifactRef),
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AnalysisArtifactRef {
    pub sha256: String,
    pub size_bytes: u64,
    pub source: AnalysisSourceRef,
    pub artifact_path: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AnalysisSourceRef {
    pub repository_relative_path: String,
    pub source_sha256: String,
    pub byte_range: Option<(u64, u64)>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AnalysisVariableInfo {
    pub name: String,
    pub kind: String,
    pub approximate_bytes: usize,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AnalysisProvenance {
    pub artifact_sha256: String,
    pub source_path: String,
    pub source_sha256: String,
    pub byte_range: Option<(u64, u64)>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AnalysisOperation {
    ByteCount,
    LineCount,
    Utf8Contains { needle: String },
    MatchingLines { needle: String, limit: usize },
    HeadLines { limit: usize },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AnalysisResult {
    pub operation: AnalysisOperation,
    pub value: AnalysisValue,
    pub provenance: Vec<AnalysisProvenance>,
    pub truncated: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AnalysisWorkspaceSnapshot {
    pub format_version: u16,
    pub session_id: String,
    pub generation: u64,
    pub variables: BTreeMap<String, AnalysisValue>,
    pub non_restorable: BTreeMap<String, String>,
    pub content_hash: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AnalysisRestoreReport {
    pub restored: Vec<String>,
    pub lost: BTreeMap<String, String>,
    pub generation: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AnalysisDelegationKind {
    ListChildren,
    FollowUp { worker_id: String, message: String },
    AwaitTerminal { worker_id: String },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AnalysisDelegationReceipt {
    pub kind: AnalysisDelegationKind,
    pub team: TeamSnapshot,
    pub authority: String,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
struct WorkspaceState {
    generation: u64,
    variables: BTreeMap<String, AnalysisValue>,
    #[serde(default)]
    non_restorable: BTreeMap<String, String>,
}

impl RuntimeController {
    /// Import a repository file into the session's immutable, content-addressed analysis store.
    /// The authoritative repository is only read; the workspace receives a copied artifact.
    pub fn analysis_import_file(
        &self,
        session_id: &str,
        repository_relative_path: &str,
        byte_range: Option<(u64, u64)>,
    ) -> Result<AnalysisArtifactRef, RuntimeError> {
        validate_session_id(session_id)?;
        let source = brokered_repository_path(&self.repo, repository_relative_path)?;
        let metadata = fs::metadata(&source).map_err(RuntimeError::agent)?;
        if !metadata.is_file() {
            return Err(RuntimeError::InvalidCommand(
                "analysis input must be a regular repository file".to_owned(),
            ));
        }
        if metadata.len() > MAX_IMPORTED_ARTIFACT_BYTES {
            return Err(RuntimeError::InvalidCommand(format!(
                "analysis artifact exceeds {MAX_IMPORTED_ARTIFACT_BYTES} bytes"
            )));
        }

        let mut all = Vec::with_capacity(metadata.len() as usize);
        File::open(&source)
            .and_then(|mut file| file.read_to_end(&mut all))
            .map_err(RuntimeError::agent)?;
        let source_sha256 = sha256_hex(&all);
        let selected = match byte_range {
            Some((start, end)) => {
                if start > end || end > all.len() as u64 {
                    return Err(RuntimeError::InvalidCommand(
                        "analysis byte range is outside the source artifact".to_owned(),
                    ));
                }
                all[start as usize..end as usize].to_vec()
            }
            None => all,
        };
        let artifact_sha256 = sha256_hex(&selected);
        let directory = artifact_directory(&self.repo, session_id);
        fs::create_dir_all(&directory).map_err(RuntimeError::agent)?;
        let artifact = directory.join(&artifact_sha256);
        if !artifact.exists() {
            atomic_write(&artifact, &selected)?;
        } else {
            let existing = fs::read(&artifact).map_err(RuntimeError::agent)?;
            if sha256_hex(&existing) != artifact_sha256 {
                return Err(RuntimeError::InvalidCommand(
                    "analysis artifact store failed content verification".to_owned(),
                ));
            }
        }

        Ok(AnalysisArtifactRef {
            sha256: artifact_sha256,
            size_bytes: selected.len() as u64,
            source: AnalysisSourceRef {
                repository_relative_path: repository_relative_path.replace('\\', "/"),
                source_sha256,
                byte_range,
            },
            artifact_path: artifact.to_string_lossy().into_owned(),
        })
    }

    /// Persist one bounded, explicitly restorable variable for this authoritative session.
    pub fn analysis_set_value(
        &self,
        session_id: &str,
        name: &str,
        value: AnalysisValue,
    ) -> Result<AnalysisVariableInfo, RuntimeError> {
        validate_session_id(session_id)?;
        validate_variable(name, &value)?;
        let mut state = load_state(&self.repo, session_id)?;
        if !state.variables.contains_key(name) && state.variables.len() >= MAX_VARIABLES {
            return Err(RuntimeError::InvalidCommand(format!(
                "analysis workspace already contains {MAX_VARIABLES} variables"
            )));
        }
        state.variables.insert(name.to_owned(), value.clone());
        state.non_restorable.remove(name);
        state.generation = state.generation.saturating_add(1);
        persist_state(&self.repo, session_id, &state)?;
        Ok(variable_info(name, &value))
    }

    pub fn analysis_list_values(
        &self,
        session_id: &str,
    ) -> Result<Vec<AnalysisVariableInfo>, RuntimeError> {
        let state = load_state(&self.repo, session_id)?;
        Ok(state
            .variables
            .iter()
            .map(|(name, value)| variable_info(name, value))
            .collect())
    }

    pub fn analysis_value(
        &self,
        session_id: &str,
        name: &str,
    ) -> Result<Option<AnalysisValue>, RuntimeError> {
        Ok(load_state(&self.repo, session_id)?
            .variables
            .get(name)
            .cloned())
    }

    /// Execute a bounded deterministic reduction over an immutable artifact. This first backend
    /// intentionally supports data reduction rather than arbitrary host-language execution.
    pub fn analysis_reduce(
        &self,
        session_id: &str,
        artifact: &AnalysisArtifactRef,
        operation: AnalysisOperation,
    ) -> Result<AnalysisResult, RuntimeError> {
        validate_session_id(session_id)?;
        let artifact_path = PathBuf::from(&artifact.artifact_path);
        let expected_directory = artifact_directory(&self.repo, session_id);
        if artifact_path.parent() != Some(expected_directory.as_path()) {
            return Err(RuntimeError::InvalidCommand(
                "analysis artifact does not belong to this session workspace".to_owned(),
            ));
        }
        let bytes = fs::read(&artifact_path).map_err(RuntimeError::agent)?;
        if sha256_hex(&bytes) != artifact.sha256 || bytes.len() as u64 != artifact.size_bytes {
            return Err(RuntimeError::InvalidCommand(
                "analysis artifact failed content verification".to_owned(),
            ));
        }

        let provenance = vec![AnalysisProvenance {
            artifact_sha256: artifact.sha256.clone(),
            source_path: artifact.source.repository_relative_path.clone(),
            source_sha256: artifact.source.source_sha256.clone(),
            byte_range: artifact.source.byte_range,
        }];
        let mut truncated = false;
        let value = match &operation {
            AnalysisOperation::ByteCount => AnalysisValue::Integer(bytes.len() as i64),
            AnalysisOperation::LineCount => AnalysisValue::Integer(
                bytes.iter().filter(|byte| **byte == b'\n').count() as i64
                    + i64::from(!bytes.is_empty() && *bytes.last().unwrap_or(&b'\n') != b'\n'),
            ),
            AnalysisOperation::Utf8Contains { needle } => {
                let text = std::str::from_utf8(&bytes).map_err(|_| {
                    RuntimeError::InvalidCommand(
                        "analysis text operation requires UTF-8".to_owned(),
                    )
                })?;
                AnalysisValue::Bool(text.contains(needle))
            }
            AnalysisOperation::MatchingLines { needle, limit } => {
                let text = std::str::from_utf8(&bytes).map_err(|_| {
                    RuntimeError::InvalidCommand(
                        "analysis text operation requires UTF-8".to_owned(),
                    )
                })?;
                let bounded = (*limit).min(MAX_SAMPLE_LINES);
                let mut matches = Vec::new();
                for line in text.lines().filter(|line| line.contains(needle)) {
                    if matches.len() >= bounded {
                        truncated = true;
                        break;
                    }
                    push_bounded_line(&mut matches, line, &mut truncated);
                    if truncated {
                        break;
                    }
                }
                AnalysisValue::StringList(matches)
            }
            AnalysisOperation::HeadLines { limit } => {
                let text = std::str::from_utf8(&bytes).map_err(|_| {
                    RuntimeError::InvalidCommand(
                        "analysis text operation requires UTF-8".to_owned(),
                    )
                })?;
                let bounded = (*limit).min(MAX_SAMPLE_LINES);
                let mut lines = Vec::new();
                for line in text.lines().take(bounded.saturating_add(1)) {
                    if lines.len() >= bounded {
                        truncated = true;
                        break;
                    }
                    push_bounded_line(&mut lines, line, &mut truncated);
                    if truncated {
                        break;
                    }
                }
                AnalysisValue::StringList(lines)
            }
        };
        Ok(AnalysisResult {
            operation,
            value,
            provenance,
            truncated,
        })
    }

    /// Record an explicitly unsupported live-language value so restart truthfully reports it lost.
    pub fn analysis_mark_non_restorable(
        &self,
        session_id: &str,
        name: &str,
        reason: &str,
    ) -> Result<(), RuntimeError> {
        if name.trim().is_empty() || reason.trim().is_empty() {
            return Err(RuntimeError::InvalidCommand(
                "non-restorable value name and reason must be non-empty".to_owned(),
            ));
        }
        let mut state = load_state(&self.repo, session_id)?;
        state.variables.remove(name);
        state
            .non_restorable
            .insert(name.to_owned(), reason.to_owned());
        state.generation = state.generation.saturating_add(1);
        persist_state(&self.repo, session_id, &state)
    }

    pub fn analysis_snapshot(
        &self,
        session_id: &str,
    ) -> Result<AnalysisWorkspaceSnapshot, RuntimeError> {
        let state = load_state(&self.repo, session_id)?;
        let mut snapshot = AnalysisWorkspaceSnapshot {
            format_version: FORMAT_VERSION,
            session_id: session_id.to_owned(),
            generation: state.generation,
            variables: state.variables,
            non_restorable: state.non_restorable,
            content_hash: String::new(),
        };
        snapshot.content_hash = snapshot_hash(&snapshot)?;
        atomic_write(
            &snapshot_path(&self.repo, session_id),
            &serde_json::to_vec_pretty(&snapshot).map_err(RuntimeError::agent)?,
        )?;
        Ok(snapshot)
    }

    pub fn analysis_restore(
        &self,
        session_id: &str,
    ) -> Result<AnalysisRestoreReport, RuntimeError> {
        let path = snapshot_path(&self.repo, session_id);
        let bytes = fs::read(&path).map_err(RuntimeError::agent)?;
        let snapshot: AnalysisWorkspaceSnapshot = match serde_json::from_slice(&bytes) {
            Ok(snapshot) => snapshot,
            Err(error) => {
                quarantine(&path)?;
                return Err(RuntimeError::InvalidCommand(format!(
                    "analysis snapshot was corrupt and quarantined: {error}"
                )));
            }
        };
        if snapshot.format_version != FORMAT_VERSION
            || snapshot.session_id != session_id
            || snapshot.content_hash != snapshot_hash(&snapshot)?
        {
            quarantine(&path)?;
            return Err(RuntimeError::InvalidCommand(
                "analysis snapshot failed identity/content verification and was quarantined"
                    .to_owned(),
            ));
        }
        for value in snapshot.variables.values() {
            validate_restored_artifact(&self.repo, session_id, value)?;
        }
        let restored = snapshot.variables.keys().cloned().collect::<Vec<_>>();
        let lost = snapshot.non_restorable.clone();
        let state = WorkspaceState {
            generation: snapshot.generation.saturating_add(1),
            variables: snapshot.variables,
            non_restorable: lost.clone(),
        };
        persist_state(&self.repo, session_id, &state)?;
        Ok(AnalysisRestoreReport {
            restored,
            lost,
            generation: state.generation,
        })
    }

    /// Recursive delegation never creates a provider/model client here. It can observe existing
    /// children or queue a typed follow-up through the canonical team control plane. Child creation
    /// itself remains the production scheduler's authority.
    pub fn analysis_delegate(
        &self,
        kind: AnalysisDelegationKind,
    ) -> Result<AnalysisDelegationReceipt, RuntimeError> {
        let team = match &kind {
            AnalysisDelegationKind::ListChildren => self.team_control.snapshot(),
            AnalysisDelegationKind::FollowUp { worker_id, message } => {
                if message.len() > MAX_DELEGATION_MESSAGE_BYTES {
                    return Err(RuntimeError::InvalidCommand(
                        "analysis delegation message exceeds bounded size".to_owned(),
                    ));
                }
                self.team_control
                    .steer(worker_id, message)
                    .map_err(RuntimeError::agent)?
            }
            AnalysisDelegationKind::AwaitTerminal { worker_id } => {
                let started = Instant::now();
                loop {
                    if self.cancel.load(Ordering::SeqCst) {
                        return Err(RuntimeError::InvalidCommand(format!(
                            "awaiting team worker `{worker_id}` was cancelled"
                        )));
                    }
                    let snapshot = self.team_control.snapshot();
                    let lifecycle = snapshot
                        .workers
                        .iter()
                        .find(|worker| worker.worker_id == *worker_id)
                        .map(|worker| worker.lifecycle)
                        .ok_or_else(|| {
                            RuntimeError::InvalidCommand(format!(
                                "unknown team worker `{worker_id}`"
                            ))
                        })?;
                    if lifecycle.is_terminal() {
                        break snapshot;
                    }
                    if started.elapsed() >= DELEGATION_AWAIT_TIMEOUT {
                        return Err(RuntimeError::InvalidCommand(format!(
                            "timed out waiting for team worker `{worker_id}` to reach a terminal state"
                        )));
                    }
                    thread::sleep(DELEGATION_AWAIT_POLL_INTERVAL);
                }
            }
        };
        Ok(AnalysisDelegationReceipt {
            kind,
            team,
            authority: "medusa-runtime/team-control".to_owned(),
        })
    }
}

fn validate_session_id(session_id: &str) -> Result<(), RuntimeError> {
    if session_id.trim().is_empty() {
        return Err(RuntimeError::InvalidCommand(
            "analysis workspace requires a stable session id".to_owned(),
        ));
    }
    Ok(())
}

fn validate_variable(name: &str, value: &AnalysisValue) -> Result<(), RuntimeError> {
    if name.trim().is_empty() || name.len() > 128 {
        return Err(RuntimeError::InvalidCommand(
            "analysis variable name must contain 1-128 bytes".to_owned(),
        ));
    }
    match value {
        AnalysisValue::Text(text) if text.len() > MAX_TEXT_VALUE_BYTES => Err(
            RuntimeError::InvalidCommand("analysis text variable exceeds bounded size".to_owned()),
        ),
        AnalysisValue::StringList(values)
            if values.iter().map(String::len).sum::<usize>() > MAX_TEXT_VALUE_BYTES =>
        {
            Err(RuntimeError::InvalidCommand(
                "analysis string-list variable exceeds bounded size".to_owned(),
            ))
        }
        _ => Ok(()),
    }
}

fn brokered_repository_path(repo: &Path, relative: &str) -> Result<PathBuf, RuntimeError> {
    let candidate = repo.join(relative);
    let canonical_repo = repo.canonicalize().map_err(RuntimeError::agent)?;
    let canonical = candidate.canonicalize().map_err(RuntimeError::agent)?;
    if !canonical.starts_with(&canonical_repo) {
        return Err(RuntimeError::InvalidCommand(
            "analysis repository read escaped the authoritative repository".to_owned(),
        ));
    }
    Ok(canonical)
}

fn workspace_root(repo: &Path, session_id: &str) -> PathBuf {
    repo.join(".medusa")
        .join("analysis-workspace-v1")
        .join(sha256_hex(session_id.as_bytes()))
}

fn artifact_directory(repo: &Path, session_id: &str) -> PathBuf {
    workspace_root(repo, session_id).join("artifacts")
}

fn state_path(repo: &Path, session_id: &str) -> PathBuf {
    workspace_root(repo, session_id).join("state.json")
}

fn snapshot_path(repo: &Path, session_id: &str) -> PathBuf {
    workspace_root(repo, session_id).join("snapshot.json")
}

fn load_state(repo: &Path, session_id: &str) -> Result<WorkspaceState, RuntimeError> {
    validate_session_id(session_id)?;
    let path = state_path(repo, session_id);
    if !path.exists() {
        return Ok(WorkspaceState::default());
    }
    let bytes = fs::read(path).map_err(RuntimeError::agent)?;
    serde_json::from_slice(&bytes).map_err(RuntimeError::agent)
}

fn persist_state(
    repo: &Path,
    session_id: &str,
    state: &WorkspaceState,
) -> Result<(), RuntimeError> {
    atomic_write(
        &state_path(repo, session_id),
        &serde_json::to_vec_pretty(state).map_err(RuntimeError::agent)?,
    )
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), RuntimeError> {
    let parent = path
        .parent()
        .ok_or_else(|| RuntimeError::agent("analysis workspace path has no parent"))?;
    fs::create_dir_all(parent).map_err(RuntimeError::agent)?;
    let temporary = path.with_extension(format!("tmp-{}", std::process::id()));
    let mut file = File::create(&temporary).map_err(RuntimeError::agent)?;
    file.write_all(bytes).map_err(RuntimeError::agent)?;
    file.sync_all().map_err(RuntimeError::agent)?;
    fs::rename(&temporary, path).map_err(RuntimeError::agent)?;
    Ok(())
}

fn quarantine(path: &Path) -> Result<(), RuntimeError> {
    let quarantine = path.with_extension("corrupt");
    if quarantine.exists() {
        fs::remove_file(&quarantine).map_err(RuntimeError::agent)?;
    }
    fs::rename(path, quarantine).map_err(RuntimeError::agent)
}

fn snapshot_hash(snapshot: &AnalysisWorkspaceSnapshot) -> Result<String, RuntimeError> {
    let mut copy = snapshot.clone();
    copy.content_hash.clear();
    Ok(sha256_hex(
        &serde_json::to_vec(&copy).map_err(RuntimeError::agent)?,
    ))
}

fn validate_restored_artifact(
    repo: &Path,
    session_id: &str,
    value: &AnalysisValue,
) -> Result<(), RuntimeError> {
    let AnalysisValue::Artifact(reference) = value else {
        return Ok(());
    };
    let path = PathBuf::from(&reference.artifact_path);
    if path.parent() != Some(artifact_directory(repo, session_id).as_path()) {
        return Err(RuntimeError::InvalidCommand(
            "analysis snapshot referenced an artifact from another workspace".to_owned(),
        ));
    }
    let bytes = fs::read(path).map_err(RuntimeError::agent)?;
    if sha256_hex(&bytes) != reference.sha256 {
        return Err(RuntimeError::InvalidCommand(
            "analysis snapshot artifact failed hash verification".to_owned(),
        ));
    }
    Ok(())
}

fn variable_info(name: &str, value: &AnalysisValue) -> AnalysisVariableInfo {
    AnalysisVariableInfo {
        name: name.to_owned(),
        kind: match value {
            AnalysisValue::Null => "null",
            AnalysisValue::Bool(_) => "bool",
            AnalysisValue::Integer(_) => "integer",
            AnalysisValue::Text(_) => "text",
            AnalysisValue::StringList(_) => "string_list",
            AnalysisValue::Artifact(_) => "artifact_ref",
        }
        .to_owned(),
        approximate_bytes: serde_json::to_vec(value).map_or(0, |bytes| bytes.len()),
    }
}

fn push_bounded_line(lines: &mut Vec<String>, line: &str, truncated: &mut bool) {
    let current = lines.iter().map(String::len).sum::<usize>();
    if current.saturating_add(line.len()) > MAX_RESULT_TEXT_BYTES {
        *truncated = true;
        return;
    }
    lines.push(line.to_owned());
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Config, TeamWorkerRegistration};
    use tempfile::TempDir;

    fn controller() -> (TempDir, RuntimeController) {
        let temp = TempDir::new().expect("tempdir");
        let controller =
            RuntimeController::start_with_config(temp.path().to_path_buf(), Config::default());
        (temp, controller)
    }

    #[test]
    fn variables_persist_and_sessions_are_isolated() {
        let (_temp, controller) = controller();
        controller
            .analysis_set_value("session-a", "count", AnalysisValue::Integer(7))
            .expect("set");
        assert_eq!(
            controller
                .analysis_value("session-a", "count")
                .expect("get"),
            Some(AnalysisValue::Integer(7))
        );
        assert_eq!(
            controller
                .analysis_value("session-b", "count")
                .expect("get"),
            None
        );
    }

    #[test]
    fn context_as_data_keeps_exact_source_provenance() {
        let (temp, controller) = controller();
        fs::write(temp.path().join("large.txt"), "alpha\nbeta\nalpha two\n").expect("fixture");
        let artifact = controller
            .analysis_import_file("session-a", "large.txt", None)
            .expect("import");
        let result = controller
            .analysis_reduce(
                "session-a",
                &artifact,
                AnalysisOperation::MatchingLines {
                    needle: "alpha".to_owned(),
                    limit: 10,
                },
            )
            .expect("reduce");
        assert_eq!(
            result.value,
            AnalysisValue::StringList(vec!["alpha".to_owned(), "alpha two".to_owned()])
        );
        assert_eq!(
            result.provenance[0].source_sha256,
            artifact.source.source_sha256
        );
        assert_eq!(result.provenance[0].artifact_sha256, artifact.sha256);
    }

    #[test]
    fn repository_escape_and_cross_session_artifacts_are_denied() {
        let (temp, controller) = controller();
        let outside = temp
            .path()
            .parent()
            .expect("parent")
            .join("outside-analysis.txt");
        fs::write(&outside, "secret").expect("outside fixture");
        assert!(
            controller
                .analysis_import_file("session-a", "../outside-analysis.txt", None)
                .is_err()
        );
        fs::write(temp.path().join("inside.txt"), "ok").expect("fixture");
        let artifact = controller
            .analysis_import_file("session-a", "inside.txt", None)
            .expect("import");
        assert!(
            controller
                .analysis_reduce("session-b", &artifact, AnalysisOperation::ByteCount)
                .is_err()
        );
        let _ = fs::remove_file(outside);
    }

    #[test]
    fn snapshot_restore_is_truthful_about_unsupported_state() {
        let (_temp, controller) = controller();
        controller
            .analysis_set_value("session-a", "count", AnalysisValue::Integer(3))
            .expect("set");
        controller
            .analysis_mark_non_restorable("session-a", "socket", "open native handle")
            .expect("mark");
        controller.analysis_snapshot("session-a").expect("snapshot");
        controller
            .analysis_set_value("session-a", "count", AnalysisValue::Integer(99))
            .expect("modify");
        let report = controller.analysis_restore("session-a").expect("restore");
        assert!(report.restored.contains(&"count".to_owned()));
        assert_eq!(
            report.lost.get("socket"),
            Some(&"open native handle".to_owned())
        );
        assert_eq!(
            controller
                .analysis_value("session-a", "count")
                .expect("get"),
            Some(AnalysisValue::Integer(3))
        );
    }

    #[test]
    fn corrupt_snapshot_is_quarantined() {
        let (_temp, controller) = controller();
        controller.analysis_snapshot("session-a").expect("snapshot");
        fs::write(snapshot_path(&controller.repo, "session-a"), b"not json").expect("corrupt");
        assert!(controller.analysis_restore("session-a").is_err());
        assert!(
            snapshot_path(&controller.repo, "session-a")
                .with_extension("corrupt")
                .exists()
        );
    }

    #[test]
    fn delegation_has_no_provider_or_scheduler_authority() {
        let (_temp, controller) = controller();
        let receipt = controller
            .analysis_delegate(AnalysisDelegationKind::ListChildren)
            .expect("list");
        assert_eq!(receipt.authority, "medusa-runtime/team-control");
        assert!(receipt.team.workers.is_empty());
        assert!(
            controller
                .analysis_delegate(AnalysisDelegationKind::FollowUp {
                    worker_id: "missing".to_owned(),
                    message: "do work".to_owned(),
                })
                .is_err()
        );
    }

    #[test]
    fn await_terminal_waits_for_terminal_worker_state() {
        let (_temp, controller) = controller();
        controller.team_control.begin(
            "execution-a",
            [TeamWorkerRegistration {
                worker_id: "worker-a".to_owned(),
                role: "analyzer".to_owned(),
                task_id: "analyze".to_owned(),
            }],
        );
        controller
            .team_control
            .start("worker-a", Some("session-a"), "running")
            .expect("start");
        let control = controller.team_control.clone();
        let completion = thread::spawn(move || {
            thread::sleep(Duration::from_millis(50));
            control.complete("worker-a", "done").expect("complete");
        });

        let receipt = controller
            .analysis_delegate(AnalysisDelegationKind::AwaitTerminal {
                worker_id: "worker-a".to_owned(),
            })
            .expect("await terminal");
        completion.join().expect("completion thread");
        let worker = receipt
            .team
            .workers
            .iter()
            .find(|worker| worker.worker_id == "worker-a")
            .expect("worker snapshot");
        assert!(worker.lifecycle.is_terminal());
    }

    #[test]
    fn await_terminal_honors_runtime_cancellation() {
        let (_temp, controller) = controller();
        controller.team_control.begin(
            "execution-a",
            [TeamWorkerRegistration {
                worker_id: "worker-a".to_owned(),
                role: "analyzer".to_owned(),
                task_id: "analyze".to_owned(),
            }],
        );
        controller.cancel.store(true, Ordering::SeqCst);
        let error = controller
            .analysis_delegate(AnalysisDelegationKind::AwaitTerminal {
                worker_id: "worker-a".to_owned(),
            })
            .expect_err("cancelled await must fail");
        assert!(error.to_string().contains("cancelled"));
    }
}
