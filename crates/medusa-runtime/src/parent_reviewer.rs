use std::{
    fs,
    path::{Path, PathBuf},
    sync::{atomic::AtomicBool, mpsc::Sender},
};

use medusa_config::Config;
use medusa_provider::{
    Message, MessageBlock, ModelProvider, ModelRequest, ResponseBlock, Role, Usage,
};
use medusa_review_model::{
    ParentReviewDecision, ParentReviewOutcome, ParentReviewResponse, ParentReviewResponseError,
    final_parent_review_line, validate_parent_review_response,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::{
    RuntimeActivity, RuntimeActivityKind, RuntimeEvent,
    mutation_transaction::{MutationTransaction, TransactionCompletion},
};

const REVIEW_JOURNAL_SCHEMA_VERSION: u16 = 1;
const REVIEW_SYSTEM_PROMPT: &str = "You are Medusa's dedicated transactional parent reviewer. You are read-only and have no tools. Evaluate only the immutable transaction packet supplied by the runtime. Do not execute the original task, inspect another repository state, ask questions, or claim integration has already occurred. Accept only when the prepared patch, changed scope, and verification evidence satisfy the scoped request. Request revision only for a concrete defect in that immutable packet. End with the exact versioned JSON envelope required by the packet and write nothing after it.";
const MAX_REVIEW_OUTPUT_TOKENS: u32 = 4_096;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum ReviewJournalState {
    Requesting,
    Completed,
    Failed,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ParentReviewJournal {
    schema_version: u16,
    reviewer_session_id: String,
    transaction_id: String,
    request_fingerprint: String,
    provider: String,
    model: String,
    state: ReviewJournalState,
    tools_advertised: bool,
    response_id: Option<String>,
    stop_reason: Option<String>,
    usage: Option<Usage>,
    execution_status: Option<Value>,
    decision: Option<ParentReviewDecision>,
    rationale: Option<String>,
    response_fingerprint: Option<String>,
    error: Option<String>,
    revision: u64,
    updated_at_unix_ms: i64,
    fingerprint: String,
}

#[derive(Clone, Debug)]
struct ReviewPacket {
    transaction_id: String,
    context: String,
    journal_path: PathBuf,
}

#[derive(Clone, Debug)]
struct DedicatedReview {
    reviewer_session_id: String,
    outcome: ParentReviewOutcome,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ParentReviewAuthorization {
    RevisionRequested(String),
    Authorized,
}

pub(crate) fn authorize<P: ModelProvider>(
    path: &Path,
    repo: &Path,
    provider: &P,
    config: &Config,
    cancel: &AtomicBool,
    events: &Sender<RuntimeEvent>,
) -> Result<ParentReviewAuthorization, String> {
    let mut transaction = MutationTransaction::open(path)?;
    let packet = ReviewPacket {
        transaction_id: transaction.snapshot().transaction_id.clone(),
        context: transaction.review_context()?,
        journal_path: path
            .parent()
            .ok_or_else(|| "mutation transaction has no durable parent directory".to_owned())?
            .join("parent-review-session.json"),
    };
    let _ = events.send(RuntimeEvent::Activity(RuntimeActivity {
        id: Some(format!("parent-review:{}", packet.transaction_id)),
        kind: RuntimeActivityKind::Progress,
        title: "Dedicated parent review".to_owned(),
        details: vec![
            "review transport advertises zero tools".to_owned(),
            format!("journal: {}", packet.journal_path.display()),
        ],
    }));
    let review = review_packet(provider, config, cancel, &packet)?;
    match transaction.record_review_decision(
        review.outcome.decision,
        review.outcome.rationale,
        &review.reviewer_session_id,
    )? {
        ParentReviewDecision::RevisionRequested => {
            let rationale = transaction
                .snapshot()
                .review
                .as_ref()
                .map(|receipt| receipt.rationale.clone())
                .unwrap_or_else(|| "parent requested revision".to_owned());
            transaction.emit(events);
            Ok(ParentReviewAuthorization::RevisionRequested(rationale))
        }
        ParentReviewDecision::Accepted => {
            transaction.emit(events);
            transaction.begin_verification()?;
            transaction.emit(events);
            transaction.verify_independently(repo)?;
            transaction.emit(events);
            transaction.authorize(repo)?;
            transaction.emit(events);
            let _ = events.send(RuntimeEvent::Activity(RuntimeActivity {
                id: Some(format!("parent-review:{}", packet.transaction_id)),
                kind: RuntimeActivityKind::Done,
                title: "Dedicated parent review authorized".to_owned(),
                details: vec![
                    format!("reviewer session {}", review.reviewer_session_id),
                    format!("response {}", review.outcome.response_fingerprint),
                    "integration remains separate from review and verification authority".to_owned(),
                ],
            }));
            Ok(ParentReviewAuthorization::Authorized)
        }
    }
}

pub(crate) fn complete<P: ModelProvider>(
    path: &Path,
    repo: &Path,
    provider: &P,
    config: &Config,
    cancel: &AtomicBool,
    events: &Sender<RuntimeEvent>,
) -> Result<TransactionCompletion, String> {
    match authorize(path, repo, provider, config, cancel, events)? {
        ParentReviewAuthorization::RevisionRequested(rationale) => {
            Ok(TransactionCompletion::RevisionRequested(rationale))
        }
        ParentReviewAuthorization::Authorized => {
            let mut transaction = MutationTransaction::open(path)?;
            transaction.integrate(repo)?;
            transaction.emit(events);
            let receipt = transaction.reconcile(repo)?;
            transaction.emit(events);
            Ok(TransactionCompletion::Reconciled(receipt))
        }
    }
}

fn review_packet<P: ModelProvider>(
    provider: &P,
    config: &Config,
    cancel: &AtomicBool,
    packet: &ReviewPacket,
) -> Result<DedicatedReview, String> {
    let request = ModelRequest {
        system: REVIEW_SYSTEM_PROMPT.to_owned(),
        messages: vec![Message {
            role: Role::User,
            content: vec![MessageBlock::Text {
                text: packet.context.clone(),
            }],
        }],
        tools: Vec::new(),
        max_tokens: config.model.max_output_tokens.min(MAX_REVIEW_OUTPUT_TOKENS),
        temperature_milli: 0,
    };
    let request_fingerprint = hash(&request);
    let reviewer_session_id = format!(
        "parent-review-{}",
        hash(&(packet.transaction_id.as_str(), request_fingerprint.as_str()))
    );

    if let Some(existing) = load_journal(&packet.journal_path)? {
        validate_journal(&existing)?;
        if existing.transaction_id != packet.transaction_id
            || existing.request_fingerprint != request_fingerprint
            || existing.reviewer_session_id != reviewer_session_id
            || existing.provider != config.model.provider
            || existing.model != config.model.name
        {
            return Err(
                "durable parent-review journal does not match this transaction request".to_owned(),
            );
        }
        match existing.state {
            ReviewJournalState::Completed => return completed_review(existing),
            ReviewJournalState::Failed => {
                return Err(existing
                    .error
                    .unwrap_or_else(|| "dedicated parent reviewer failed closed".to_owned()));
            }
            ReviewJournalState::Requesting => {}
        }
    }

    let revision = load_journal(&packet.journal_path)?
        .map(|journal| journal.revision.saturating_add(1))
        .unwrap_or_default();
    let mut journal = ParentReviewJournal {
        schema_version: REVIEW_JOURNAL_SCHEMA_VERSION,
        reviewer_session_id: reviewer_session_id.clone(),
        transaction_id: packet.transaction_id.clone(),
        request_fingerprint,
        provider: config.model.provider.clone(),
        model: config.model.name.clone(),
        state: ReviewJournalState::Requesting,
        tools_advertised: false,
        response_id: None,
        stop_reason: None,
        usage: None,
        execution_status: None,
        decision: None,
        rationale: None,
        response_fingerprint: None,
        error: None,
        revision,
        updated_at_unix_ms: now_unix_ms(),
        fingerprint: String::new(),
    };
    persist_journal(&packet.journal_path, &mut journal)?;

    let response = match provider.complete_cancellable(&request, cancel) {
        Ok(response) => response,
        Err(error) => {
            let message = error.to_string();
            fail_journal(
                &packet.journal_path,
                &mut journal,
                message.clone(),
                provider.execution_status(),
            )?;
            return Err(message);
        }
    };
    journal.response_id = response.response_id.clone();
    journal.stop_reason = response.stop_reason.clone();
    journal.usage = Some(response.usage);
    journal.execution_status = provider.execution_status();

    let text = match response_text(&response.blocks) {
        Ok(text) => text,
        Err(error) => {
            fail_journal(
                &packet.journal_path,
                &mut journal,
                error.clone(),
                provider.execution_status(),
            )?;
            return Err(error);
        }
    };
    let outcome = match decode_parent_review_response(&text) {
        Ok(outcome) => outcome,
        Err(error) => {
            fail_journal(
                &packet.journal_path,
                &mut journal,
                error.clone(),
                provider.execution_status(),
            )?;
            return Err(error);
        }
    };
    journal.state = ReviewJournalState::Completed;
    journal.decision = Some(outcome.decision.clone());
    journal.rationale = Some(outcome.rationale.clone());
    journal.response_fingerprint = Some(outcome.response_fingerprint.clone());
    journal.error = None;
    persist_journal(&packet.journal_path, &mut journal)?;
    Ok(DedicatedReview {
        reviewer_session_id,
        outcome,
    })
}

fn completed_review(journal: ParentReviewJournal) -> Result<DedicatedReview, String> {
    let outcome = ParentReviewOutcome {
        schema_version: medusa_review_model::PARENT_REVIEW_SCHEMA_VERSION,
        decision: journal
            .decision
            .ok_or_else(|| "completed parent-review journal has no decision".to_owned())?,
        rationale: journal
            .rationale
            .ok_or_else(|| "completed parent-review journal has no rationale".to_owned())?,
        response_fingerprint: journal.response_fingerprint.ok_or_else(|| {
            "completed parent-review journal has no response fingerprint".to_owned()
        })?,
    };
    Ok(DedicatedReview {
        reviewer_session_id: journal.reviewer_session_id,
        outcome,
    })
}

fn response_text(blocks: &[ResponseBlock]) -> Result<String, String> {
    let mut text = Vec::new();
    for block in blocks {
        match block {
            ResponseBlock::Text { text: value } => text.push(value.as_str()),
            ResponseBlock::ToolUse { name, .. } => {
                return Err(format!(
                    "dedicated parent reviewer attempted forbidden tool `{name}`"
                ));
            }
        }
    }
    let text = text.join("\n");
    if text.trim().is_empty() {
        return Err("dedicated parent reviewer produced no text".to_owned());
    }
    Ok(text)
}

fn decode_parent_review_response(text: &str) -> Result<ParentReviewOutcome, String> {
    let final_line = final_parent_review_line(text).map_err(|error| error.to_string())?;
    let response: ParentReviewResponse = serde_json::from_str(final_line).map_err(|error| {
        ParentReviewResponseError::InvalidEnvelope(error.to_string()).to_string()
    })?;
    validate_parent_review_response(response, final_line).map_err(|error| error.to_string())
}

fn fail_journal(
    path: &Path,
    journal: &mut ParentReviewJournal,
    error: String,
    execution_status: Option<Value>,
) -> Result<(), String> {
    journal.state = ReviewJournalState::Failed;
    journal.execution_status = execution_status;
    journal.error = Some(error);
    persist_journal(path, journal)
}

fn load_journal(path: &Path) -> Result<Option<ParentReviewJournal>, String> {
    if !path.exists() {
        return Ok(None);
    }
    let contents = fs::read_to_string(path).map_err(|error| error.to_string())?;
    serde_json::from_str(&contents)
        .map(Some)
        .map_err(|error| format!("parent-review journal is corrupt: {error}"))
}

fn persist_journal(path: &Path, journal: &mut ParentReviewJournal) -> Result<(), String> {
    journal.updated_at_unix_ms = now_unix_ms();
    journal.fingerprint.clear();
    journal.fingerprint = hash(&*journal);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let temporary = path.with_extension("json.tmp");
    let encoded = serde_json::to_vec_pretty(journal).map_err(|error| error.to_string())?;
    fs::write(&temporary, encoded).map_err(|error| error.to_string())?;
    replace_file(&temporary, path)
}

fn replace_file(temporary: &Path, destination: &Path) -> Result<(), String> {
    match fs::rename(temporary, destination) {
        Ok(()) => Ok(()),
        Err(first_error) if destination.exists() => {
            fs::remove_file(destination).map_err(|error| {
                format!(
                    "could not replace durable parent-review journal after {first_error}: {error}"
                )
            })?;
            fs::rename(temporary, destination).map_err(|error| error.to_string())
        }
        Err(error) => Err(error.to_string()),
    }
}

fn validate_journal(journal: &ParentReviewJournal) -> Result<(), String> {
    let mut canonical = journal.clone();
    let fingerprint = canonical.fingerprint.clone();
    canonical.fingerprint.clear();
    if journal.schema_version != REVIEW_JOURNAL_SCHEMA_VERSION
        || journal.reviewer_session_id.trim().is_empty()
        || journal.transaction_id.trim().is_empty()
        || journal.request_fingerprint.trim().is_empty()
        || journal.provider.trim().is_empty()
        || journal.model.trim().is_empty()
        || journal.tools_advertised
        || fingerprint != hash(&canonical)
    {
        return Err("durable parent-review journal is incomplete or corrupted".to_owned());
    }
    match journal.state {
        ReviewJournalState::Requesting => {
            if journal.decision.is_some()
                || journal.rationale.is_some()
                || journal.response_fingerprint.is_some()
                || journal.error.is_some()
            {
                return Err("requesting parent-review journal contains terminal data".to_owned());
            }
        }
        ReviewJournalState::Completed => {
            if journal.decision.is_none()
                || is_blank(journal.rationale.as_deref())
                || is_blank(journal.response_fingerprint.as_deref())
                || journal.error.is_some()
            {
                return Err("completed parent-review journal is missing typed evidence".to_owned());
            }
        }
        ReviewJournalState::Failed => {
            if is_blank(journal.error.as_deref()) || journal.decision.is_some() {
                return Err("failed parent-review journal is missing failure evidence".to_owned());
            }
        }
    }
    Ok(())
}

fn is_blank(value: Option<&str>) -> bool {
    value.is_none_or(|value| value.trim().is_empty())
}

fn hash(value: &impl Serialize) -> String {
    let encoded = serde_json::to_vec(value).unwrap_or_default();
    format!("{:x}", Sha256::digest(encoded))
}

fn now_unix_ms() -> i64 {
    time::OffsetDateTime::now_utc()
        .unix_timestamp_nanos()
        .checked_div(1_000_000)
        .and_then(|value| i64::try_from(value).ok())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    };

    use medusa_core::MedusaResult;
    use medusa_provider::ModelResponse;
    use tempfile::tempdir;

    use super::*;

    struct FakeProvider {
        response: ModelResponse,
        calls: AtomicUsize,
        request: Mutex<Option<ModelRequest>>,
    }

    impl FakeProvider {
        fn text(text: &str) -> Self {
            Self {
                response: ModelResponse {
                    response_id: Some("response-1".to_owned()),
                    stop_reason: Some("end_turn".to_owned()),
                    blocks: vec![ResponseBlock::Text {
                        text: text.to_owned(),
                    }],
                    usage: Usage::default(),
                },
                calls: AtomicUsize::new(0),
                request: Mutex::new(None),
            }
        }
    }

    impl ModelProvider for FakeProvider {
        fn complete(&self, request: &ModelRequest) -> MedusaResult<ModelResponse> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            *self.request.lock().expect("request lock") = Some(request.clone());
            Ok(self.response.clone())
        }
    }

    fn packet(root: &Path) -> ReviewPacket {
        ReviewPacket {
            transaction_id: "transaction-1".to_owned(),
            context: "immutable review packet".to_owned(),
            journal_path: root.join("parent-review-session.json"),
        }
    }

    #[test]
    fn advertises_no_tools_and_resumes_completed_journal_without_second_call() {
        let root = tempdir().expect("temporary journal");
        let provider = FakeProvider::text(
            "{\"schema_version\":1,\"decision\":\"accepted\",\"rationale\":\"patch and evidence agree\"}",
        );
        let config = Config::default();
        let cancel = AtomicBool::new(false);
        let first =
            review_packet(&provider, &config, &cancel, &packet(root.path())).expect("first review");
        let second = review_packet(&provider, &config, &cancel, &packet(root.path()))
            .expect("resumed review");
        assert_eq!(first.outcome.decision, ParentReviewDecision::Accepted);
        assert_eq!(first.reviewer_session_id, second.reviewer_session_id);
        assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
        let request = provider
            .request
            .lock()
            .expect("request lock")
            .clone()
            .expect("captured request");
        assert!(request.tools.is_empty());
        assert_eq!(request.temperature_milli, 0);
    }

    #[test]
    fn typed_parent_review_envelope_is_required_at_runtime_boundary() {
        let invalid = [
            "MEDUSA_REVIEW_ACCEPTED: exact patch and evidence agree",
            "{\"schema_version\":1,\"decision\":\"accepted\",\"rationale\":\"ok\",\"extra\":true}",
            "{\"schema_version\":1,\"decision\":\"accepted\",\"rationale\":\"ok\"}\ntrailing",
        ];
        for response in invalid {
            let root = tempdir().expect("temporary journal");
            let provider = FakeProvider::text(response);
            let error = review_packet(
                &provider,
                &Config::default(),
                &AtomicBool::new(false),
                &packet(root.path()),
            )
            .expect_err("invalid review response must fail closed");
            assert!(!error.trim().is_empty());
            let journal = load_journal(&packet(root.path()).journal_path)
                .expect("journal read")
                .expect("journal");
            assert_eq!(journal.state, ReviewJournalState::Failed);
        }
    }

    #[test]
    fn tool_use_fails_closed_and_is_durable() {
        let root = tempdir().expect("temporary journal");
        let provider = FakeProvider {
            response: ModelResponse {
                response_id: Some("response-2".to_owned()),
                stop_reason: Some("tool_use".to_owned()),
                blocks: vec![ResponseBlock::ToolUse {
                    id: "tool-1".to_owned(),
                    name: "shell".to_owned(),
                    input: serde_json::json!({}),
                }],
                usage: Usage::default(),
            },
            calls: AtomicUsize::new(0),
            request: Mutex::new(None),
        };
        let error = review_packet(
            &provider,
            &Config::default(),
            &AtomicBool::new(false),
            &packet(root.path()),
        )
        .expect_err("tool use must fail");
        assert!(error.contains("forbidden tool"));
        let journal = load_journal(&packet(root.path()).journal_path)
            .expect("journal read")
            .expect("journal");
        assert_eq!(journal.state, ReviewJournalState::Failed);
        assert!(!journal.tools_advertised);
    }

    #[test]
    fn corrupt_terminal_journal_fails_closed() {
        let root = tempdir().expect("temporary journal");
        let provider = FakeProvider::text(
            "{\"schema_version\":1,\"decision\":\"accepted\",\"rationale\":\"ok\"}",
        );
        let config = Config::default();
        let cancel = AtomicBool::new(false);
        review_packet(&provider, &config, &cancel, &packet(root.path())).expect("first review");
        let path = packet(root.path()).journal_path;
        let mut journal = load_journal(&path)
            .expect("journal read")
            .expect("journal");
        journal.rationale = Some(" ".to_owned());
        fs::write(&path, serde_json::to_vec_pretty(&journal).expect("encode journal"))
            .expect("corrupt journal");
        let error = review_packet(&provider, &config, &cancel, &packet(root.path()))
            .expect_err("corrupt journal must fail");
        assert!(error.contains("incomplete or corrupted"));
        assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
    }
}
