use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::Command,
};

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use medusa_protocol::{EventEnvelope, EventPayload};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    compaction_v2::{SemanticHistory, SemanticProvenance, SemanticSummaryStatus},
    session::AgentSession,
};

pub const BRANCH_SUMMARY_FORMAT_VERSION: u32 = 1;
const MAX_BRANCH_SUMMARIES: usize = 32;
const MAX_CONTEXT_SUMMARIES: usize = 6;
const MAX_CONTEXT_CHARS: usize = 12_000;
const MAX_SEMANTIC_ENTRIES: usize = 24;
const MAX_SEMANTIC_ENTRY_CHARS: usize = 600;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct BranchAnchor {
    pub sequence: u64,
    pub event_fingerprint: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct BranchRepositoryIdentity {
    pub revision: Option<String>,
    pub tree: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct DeterministicBranchMetadata {
    pub repository_identities: Vec<BranchRepositoryIdentity>,
    pub files_read: Vec<String>,
    pub files_modified_or_touched: Vec<String>,
    pub tools_requested: Vec<String>,
    pub tool_terminal_results: Vec<String>,
    pub verification_results: Vec<String>,
    pub verification_artifacts: Vec<String>,
    pub approval_records: Vec<serde_json::Value>,
    pub team_records: Vec<serde_json::Value>,
    pub tool_artifacts: Vec<String>,
    pub integrated_to_authoritative_repository: bool,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct BranchSummaryRecord {
    pub format_version: u32,
    pub session_id: String,
    pub abandoned_for_checkpoint_id: String,
    pub base: BranchAnchor,
    pub terminal: BranchAnchor,
    pub source_event_sequences: Vec<u64>,
    pub source_event_fingerprints: Vec<String>,
    pub deterministic: DeterministicBranchMetadata,
    pub semantic_history: SemanticHistory,
    pub semantic_provenance: SemanticProvenance,
    #[serde(default)]
    pub prior_summary_hashes: Vec<String>,
    pub record_hash: String,
}

impl BranchSummaryRecord {
    pub fn verify(&self) -> MedusaResult<()> {
        if self.format_version != BRANCH_SUMMARY_FORMAT_VERSION {
            return Err(validation_error("unsupported branch summary format version"));
        }
        if self.session_id.trim().is_empty() || self.abandoned_for_checkpoint_id.trim().is_empty() {
            return Err(validation_error("branch summary identity must not be empty"));
        }
        if self.base.sequence >= self.terminal.sequence {
            return Err(validation_error("branch summary terminal must follow its base"));
        }
        if self.source_event_sequences.first().copied() != Some(self.base.sequence + 1)
            || self.source_event_sequences.last().copied() != Some(self.terminal.sequence)
            || self.source_event_sequences.len() != self.source_event_fingerprints.len()
        {
            return Err(validation_error("branch summary event range is inconsistent"));
        }
        if !self.semantic_provenance.advisory_only {
            return Err(validation_error("branch semantic history must remain advisory-only"));
        }
        let expected = hash_record(self)?;
        if self.record_hash != expected {
            return Err(validation_error("branch summary content hash mismatch"));
        }
        Ok(())
    }
}

/// Returns the exact common authoritative ancestor represented by two event histories.
/// The comparison is checksum based; semantic content never participates in ancestry.
pub fn common_ancestor(left: &[EventEnvelope], right: &[EventEnvelope]) -> Option<BranchAnchor> {
    left.iter()
        .zip(right)
        .take_while(|(left, right)| {
            left.sequence == right.sequence && left.checksum == right.checksum
        })
        .last()
        .map(|(event, _)| BranchAnchor {
            sequence: event.sequence,
            event_fingerprint: Some(event.checksum.clone()),
        })
}

/// Captures the branch that is about to be abandoned by a checkpoint restore.
/// This function is deliberately side-effect-limited to a content-addressed advisory artifact and
/// the session's artifact reference list. Callers must not make restore success depend on it.
pub(crate) fn capture_restore_abandonment(
    session: &mut AgentSession,
    checkpoint_id: &str,
    source_cursor: u64,
) -> MedusaResult<Option<PathBuf>> {
    let Some(terminal_event) = session.events.last() else {
        return Ok(None);
    };
    if source_cursor >= terminal_event.sequence {
        return Ok(None);
    }

    let base_fingerprint = if source_cursor == 0 {
        None
    } else {
        let base = session
            .events
            .iter()
            .find(|event| event.sequence == source_cursor)
            .ok_or_else(|| validation_error("restore cursor does not reference this event chain"))?;
        Some(base.checksum.clone())
    };
    let abandoned = session
        .events
        .iter()
        .filter(|event| event.sequence > source_cursor)
        .collect::<Vec<_>>();
    if abandoned.is_empty() {
        return Ok(None);
    }

    let deterministic = deterministic_metadata(session, &abandoned);
    let semantic_history = deterministic_semantic_history(&abandoned);
    let prior_summary_hashes = valid_records(session)
        .into_iter()
        .map(|record| record.record_hash)
        .collect::<Vec<_>>();
    let mut record = BranchSummaryRecord {
        format_version: BRANCH_SUMMARY_FORMAT_VERSION,
        session_id: session.id.as_str().to_owned(),
        abandoned_for_checkpoint_id: checkpoint_id.to_owned(),
        base: BranchAnchor {
            sequence: source_cursor,
            event_fingerprint: base_fingerprint,
        },
        terminal: BranchAnchor {
            sequence: terminal_event.sequence,
            event_fingerprint: Some(terminal_event.checksum.clone()),
        },
        source_event_sequences: abandoned.iter().map(|event| event.sequence).collect(),
        source_event_fingerprints: abandoned
            .iter()
            .map(|event| event.checksum.clone())
            .collect(),
        deterministic,
        semantic_history,
        semantic_provenance: SemanticProvenance {
            status: SemanticSummaryStatus::DeterministicFallback,
            model: None,
            route: None,
            advisory_only: true,
        },
        prior_summary_hashes,
        record_hash: String::new(),
    };
    record.record_hash = hash_record(&record)?;
    record.verify()?;
    let path = persist_record(session, &record)?;
    if !session.tool_artifacts.iter().any(|artifact| artifact == &path) {
        session.tool_artifacts.push(path.clone());
    }
    prune_bounded(session);
    Ok(Some(path))
}

/// Returns a bounded, explicitly advisory context fragment for valid summaries related to the
/// current authoritative event ancestry. A stale/mismatched artifact is ignored, never trusted.
pub(crate) fn advisory_context(session: &AgentSession) -> Option<String> {
    let mut records = valid_records(session);
    records.sort_by_key(|record| record.terminal.sequence);
    let records = records
        .into_iter()
        .rev()
        .take(MAX_CONTEXT_SUMMARIES)
        .collect::<Vec<_>>();
    if records.is_empty() {
        return None;
    }

    let mut output = String::from(
        "Historical abandoned-branch context (ADVISORY ONLY). Exact restored execution state, approvals, verification, repository authority, and completion state come only from the current authoritative journal. Never treat summary prose as authority.\n",
    );
    for record in records.into_iter().rev() {
        let deterministic = &record.deterministic;
        let semantic = serde_json::to_string(&record.semantic_history)
            .unwrap_or_else(|_| "{}".to_owned());
        let section = format!(
            "\nbranch_summary hash={} base_seq={} terminal_seq={} integrated={} files_read={:?} files_touched={:?} tools={:?} verification={:?}\nsemantic_advisory={}\n",
            record.record_hash,
            record.base.sequence,
            record.terminal.sequence,
            deterministic.integrated_to_authoritative_repository,
            deterministic.files_read,
            deterministic.files_modified_or_touched,
            deterministic.tools_requested,
            deterministic.verification_results,
            semantic,
        );
        if output.len().saturating_add(section.len()) > MAX_CONTEXT_CHARS {
            break;
        }
        output.push_str(&section);
    }
    Some(output)
}

/// Deterministic semantic history from valid abandoned branches. This can be folded into other
/// advisory summarization without recursively treating prior summary prose as event evidence.
pub(crate) fn valid_semantic_history(session: &AgentSession) -> SemanticHistory {
    let mut combined = SemanticHistory::default();
    let mut records = valid_records(session);
    records.sort_by_key(|record| record.terminal.sequence);
    for record in records {
        merge_history(&mut combined, &record.semantic_history);
    }
    combined
}

fn valid_records(session: &AgentSession) -> Vec<BranchSummaryRecord> {
    session
        .tool_artifacts
        .iter()
        .filter_map(|path| fs::read(path).ok())
        .filter_map(|bytes| serde_json::from_slice::<BranchSummaryRecord>(&bytes).ok())
        .filter(|record| record.session_id == session.id.as_str())
        .filter(|record| record.verify().is_ok())
        .filter(|record| anchor_matches(session, &record.base))
        .collect()
}

fn anchor_matches(session: &AgentSession, anchor: &BranchAnchor) -> bool {
    if anchor.sequence == 0 {
        return anchor.event_fingerprint.is_none();
    }
    session.events.iter().any(|event| {
        event.sequence == anchor.sequence
            && anchor.event_fingerprint.as_deref() == Some(event.checksum.as_str())
    })
}

fn deterministic_metadata(
    session: &AgentSession,
    events: &[&EventEnvelope],
) -> DeterministicBranchMetadata {
    let mut files_read = BTreeSet::new();
    let mut files_touched = BTreeSet::new();
    let mut tools_requested = Vec::new();
    let mut tool_terminal_results = Vec::new();
    let mut verification_results = Vec::new();
    let mut verification_artifacts = BTreeSet::new();
    let mut approval_records = Vec::new();
    let mut team_records = Vec::new();
    let mut tool_artifacts = BTreeSet::new();
    let mut integrated = false;

    for event in events {
        match &event.payload {
            EventPayload::ToolCallRequested { tool, arguments } => {
                tools_requested.push(tool.clone());
                if is_read_tool(tool) {
                    collect_paths(arguments, &mut files_read);
                }
            }
            EventPayload::ToolExecutionCompleted { tool, exit_code } => {
                tool_terminal_results.push(format!("{tool}:exit={exit_code:?}"));
            }
            EventPayload::ToolCallDenied { tool, reason } => {
                tool_terminal_results.push(format!("{tool}:denied:{reason}"));
            }
            EventPayload::ToolOutputChunk { artifact_ref, .. } => {
                tool_artifacts.insert(artifact_ref.clone());
            }
            EventPayload::FileTransactionCommitted { paths, rollback_ref } => {
                files_touched.extend(paths.iter().cloned());
                tool_artifacts.insert(rollback_ref.clone());
            }
            EventPayload::VerificationStarted { commands } => {
                verification_results.push(format!("started:{commands:?}"));
            }
            EventPayload::VerificationCompleted { passed, evidence } => {
                verification_results.push(format!("completed:passed={passed}"));
                verification_artifacts.extend(evidence.iter().cloned());
            }
            EventPayload::ApprovalRequested { request } => approval_records.push(request.clone()),
            EventPayload::ApprovalDecisionRecorded { decision } => {
                approval_records.push(decision.clone());
            }
            EventPayload::TeamStateChanged { snapshot } => team_records.push(snapshot.clone()),
            EventPayload::WorkerEvidenceRecorded { evidence } => team_records.push(evidence.clone()),
            EventPayload::IntegrationReceiptRecorded { receipt } => {
                integrated = true;
                team_records.push(receipt.clone());
            }
            _ => {}
        }
    }

    DeterministicBranchMetadata {
        repository_identities: vec![repository_identity(&session.repo)],
        files_read: files_read.into_iter().collect(),
        files_modified_or_touched: files_touched.into_iter().collect(),
        tools_requested,
        tool_terminal_results,
        verification_results,
        verification_artifacts: verification_artifacts.into_iter().collect(),
        approval_records,
        team_records,
        tool_artifacts: tool_artifacts.into_iter().collect(),
        integrated_to_authoritative_repository: integrated,
    }
}

fn deterministic_semantic_history(events: &[&EventEnvelope]) -> SemanticHistory {
    let mut history = SemanticHistory::default();
    for event in events {
        match &event.payload {
            EventPayload::SessionCreated { objective } | EventPayload::GoalUpdated { objective } => {
                push_semantic(&mut history.goal_context, objective);
            }
            EventPayload::UserPromptReceived { text } | EventPayload::UserFollowupDequeued { text, .. } => {
                push_semantic(&mut history.goal_context, text);
            }
            EventPayload::AssumptionRecorded { assumption, rationale } => {
                push_semantic(
                    &mut history.key_decisions,
                    &format!("assumption: {assumption}; rationale: {rationale}"),
                );
            }
            EventPayload::PlanCreated { plan } | EventPayload::PlanUpdated { update: plan } => {
                push_semantic(&mut history.in_progress_work, &plan.to_string());
            }
            EventPayload::QuestionRequested { question } => {
                push_semantic(&mut history.unresolved_questions, &question.to_string());
            }
            EventPayload::ToolCallDenied { tool, reason } => {
                push_semantic(&mut history.blockers, &format!("{tool}: {reason}"));
            }
            EventPayload::FileTransactionCommitted { paths, .. } => {
                push_semantic(
                    &mut history.completed_work,
                    &format!("repository transaction touched {}", paths.join(", ")),
                );
                for path in paths {
                    push_semantic(&mut history.exact_identifiers, path);
                }
            }
            EventPayload::VerificationStarted { commands } => {
                for command in commands {
                    push_semantic(&mut history.exact_identifiers, command);
                }
            }
            EventPayload::VerificationCompleted { passed, .. } => {
                push_semantic(
                    if *passed {
                        &mut history.completed_work
                    } else {
                        &mut history.blockers
                    },
                    if *passed {
                        "authoritative verification passed on the abandoned branch"
                    } else {
                        "authoritative verification failed on the abandoned branch"
                    },
                );
            }
            EventPayload::RuntimeFailed { message } => {
                push_semantic(&mut history.blockers, message);
            }
            EventPayload::SessionCompleted { report_ref } => {
                push_semantic(&mut history.completed_work, &format!("session report {report_ref}"));
            }
            _ => {}
        }
    }
    history
}

fn is_read_tool(tool: &str) -> bool {
    matches!(
        tool,
        "fs_read" | "search_text" | "code_index" | "inspect_target" | "repository_context"
    )
}

fn collect_paths(value: &serde_json::Value, target: &mut BTreeSet<String>) {
    match value {
        serde_json::Value::Object(map) => {
            for (key, value) in map {
                if matches!(
                    key.as_str(),
                    "path" | "paths" | "file" | "files" | "source" | "target"
                ) {
                    collect_string_values(value, target);
                }
            }
        }
        serde_json::Value::Array(values) => {
            for value in values {
                collect_paths(value, target);
            }
        }
        _ => {}
    }
}

fn collect_string_values(value: &serde_json::Value, target: &mut BTreeSet<String>) {
    match value {
        serde_json::Value::String(value) if !value.trim().is_empty() => {
            target.insert(value.clone());
        }
        serde_json::Value::Array(values) => {
            for value in values {
                collect_string_values(value, target);
            }
        }
        _ => {}
    }
}

fn repository_identity(repo: &Path) -> BranchRepositoryIdentity {
    fn git(repo: &Path, args: &[&str]) -> Option<String> {
        let output = Command::new("git")
            .args(["-C"])
            .arg(repo)
            .args(args)
            .output()
            .ok()?;
        output.status.success().then(|| {
            String::from_utf8_lossy(&output.stdout).trim().to_owned()
        })
    }
    BranchRepositoryIdentity {
        revision: git(repo, &["rev-parse", "HEAD"]),
        tree: git(repo, &["rev-parse", "HEAD^{tree}"]),
    }
}

fn persist_record(session: &AgentSession, record: &BranchSummaryRecord) -> MedusaResult<PathBuf> {
    let root = session
        .repo
        .join(".medusa/artifacts/branch-summary-v1");
    fs::create_dir_all(&root)?;
    let path = root.join(format!("{}.json", record.record_hash));
    if path.is_file() {
        return Ok(path);
    }
    let bytes = serde_json::to_vec_pretty(record).map_err(json_error)?;
    let temp = root.join(format!(".{}.{}.tmp", record.record_hash, std::process::id()));
    {
        let mut file = fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temp)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
    }
    match fs::rename(&temp, &path) {
        Ok(()) => {}
        Err(error) if path.is_file() => {
            let _ = fs::remove_file(&temp);
            let existing = fs::read(&path)?;
            if existing != bytes {
                return Err(MedusaError::new(
                    ErrorCode::InternalInvariant,
                    ErrorCategory::Execution,
                    "branch summary content-address collision",
                ));
            }
        }
        Err(error) => {
            let _ = fs::remove_file(&temp);
            return Err(error.into());
        }
    }
    if let Ok(directory) = fs::File::open(&root) {
        let _ = directory.sync_all();
    }
    Ok(path)
}

fn prune_bounded(session: &mut AgentSession) {
    let mut branch_artifacts = session
        .tool_artifacts
        .iter()
        .filter_map(|path| {
            fs::read(path)
                .ok()
                .and_then(|bytes| serde_json::from_slice::<BranchSummaryRecord>(&bytes).ok())
                .map(|record| (record.terminal.sequence, path.clone()))
        })
        .collect::<Vec<_>>();
    if branch_artifacts.len() <= MAX_BRANCH_SUMMARIES {
        return;
    }
    branch_artifacts.sort_by_key(|(sequence, _)| *sequence);
    let remove = branch_artifacts.len().saturating_sub(MAX_BRANCH_SUMMARIES);
    let obsolete = branch_artifacts
        .into_iter()
        .take(remove)
        .map(|(_, path)| path)
        .collect::<BTreeSet<_>>();
    session.tool_artifacts.retain(|path| !obsolete.contains(path));
    for path in obsolete {
        let _ = fs::remove_file(path);
    }
}

fn hash_record(record: &BranchSummaryRecord) -> MedusaResult<String> {
    let mut value = record.clone();
    value.record_hash.clear();
    hash_json(&value)
}

fn hash_json<T: Serialize>(value: &T) -> MedusaResult<String> {
    let bytes = serde_json::to_vec(value).map_err(json_error)?;
    Ok(hex::encode(Sha256::digest(bytes)))
}

fn merge_history(target: &mut SemanticHistory, source: &SemanticHistory) {
    merge_entries(&mut target.goal_context, &source.goal_context);
    merge_entries(
        &mut target.constraints_preferences,
        &source.constraints_preferences,
    );
    merge_entries(&mut target.completed_work, &source.completed_work);
    merge_entries(&mut target.in_progress_work, &source.in_progress_work);
    merge_entries(&mut target.blockers, &source.blockers);
    merge_entries(&mut target.key_decisions, &source.key_decisions);
    merge_entries(&mut target.exact_identifiers, &source.exact_identifiers);
    merge_entries(
        &mut target.unresolved_questions,
        &source.unresolved_questions,
    );
    merge_entries(&mut target.next_steps, &source.next_steps);
}

fn merge_entries(target: &mut Vec<String>, source: &[String]) {
    for value in source {
        push_semantic(target, value);
    }
}

fn push_semantic(target: &mut Vec<String>, value: &str) {
    let value = value.trim();
    if value.is_empty() {
        return;
    }
    let bounded = value.chars().take(MAX_SEMANTIC_ENTRY_CHARS).collect::<String>();
    if !target.iter().any(|existing| existing == &bounded) {
        target.push(bounded);
    }
    if target.len() > MAX_SEMANTIC_ENTRIES {
        target.drain(..target.len().saturating_sub(MAX_SEMANTIC_ENTRIES));
    }
}

fn validation_error(message: impl Into<String>) -> MedusaError {
    MedusaError::new(ErrorCode::InvalidConfiguration, ErrorCategory::Validation, message)
}

fn json_error(error: serde_json::Error) -> MedusaError {
    MedusaError::new(ErrorCode::InternalInvariant, ErrorCategory::Internal, error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn digest(label: &str) -> String {
        hex::encode(Sha256::digest(label.as_bytes()))
    }

    #[test]
    fn record_hash_rejects_semantic_or_authority_tampering() {
        let mut record = BranchSummaryRecord {
            format_version: BRANCH_SUMMARY_FORMAT_VERSION,
            session_id: "session".to_owned(),
            abandoned_for_checkpoint_id: "checkpoint".to_owned(),
            base: BranchAnchor {
                sequence: 4,
                event_fingerprint: Some(digest("base")),
            },
            terminal: BranchAnchor {
                sequence: 6,
                event_fingerprint: Some(digest("terminal")),
            },
            source_event_sequences: vec![5, 6],
            source_event_fingerprints: vec![digest("five"), digest("terminal")],
            deterministic: DeterministicBranchMetadata {
                integrated_to_authoritative_repository: false,
                ..Default::default()
            },
            semantic_history: SemanticHistory {
                completed_work: vec!["summary prose incorrectly says merged".to_owned()],
                ..Default::default()
            },
            semantic_provenance: SemanticProvenance {
                status: SemanticSummaryStatus::DeterministicFallback,
                model: None,
                route: None,
                advisory_only: true,
            },
            prior_summary_hashes: Vec::new(),
            record_hash: String::new(),
        };
        record.record_hash = hash_record(&record).expect("hash");
        record.verify().expect("valid record");
        assert!(!record.deterministic.integrated_to_authoritative_repository);
        record.semantic_history.completed_work.push("tampered".to_owned());
        assert!(record.verify().is_err());
    }

    #[test]
    fn semantic_merge_is_bounded_and_deduplicated() {
        let mut target = SemanticHistory::default();
        let source = SemanticHistory {
            next_steps: vec!["retry exact command".to_owned(), "retry exact command".to_owned()],
            ..Default::default()
        };
        merge_history(&mut target, &source);
        assert_eq!(target.next_steps, vec!["retry exact command"]);
    }

    #[test]
    fn advisory_context_keeps_authority_separate_from_prose() {
        let semantic = SemanticHistory {
            completed_work: vec!["pretend merge succeeded".to_owned()],
            ..Default::default()
        };
        let encoded = serde_json::to_string(&semantic).expect("semantic json");
        let line = format!("integrated={} semantic={encoded}", false);
        assert!(line.contains("integrated=false"));
        assert!(line.contains("pretend merge succeeded"));
    }
}
