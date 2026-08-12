//! Frontend-neutral learning review, privacy, and tamper-evident audit state.

use std::{
    collections::BTreeSet,
    fs::{self, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const SCHEMA_VERSION: u32 = 1;
const ZERO_HASH: &str = "0000000000000000000000000000000000000000000000000000000000000000";

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LearningReviewState {
    Proposed,
    Deferred,
    Approved,
    Rejected,
    Validated,
    Active,
    Suspended,
    RolledBack,
    Deleted,
    Conflict,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LearningKind {
    SessionFact,
    RepositoryLearning,
    UserPreference,
    Skill,
    Policy,
    ProductCodeChange,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct LearningPrivacy {
    pub capture_enabled: bool,
    pub user_persistence_enabled: bool,
    pub cross_repository_reuse_enabled: bool,
    pub telemetry_enabled: bool,
    pub automatic_proposals_enabled: bool,
}

impl LearningPrivacy {
    #[must_use]
    pub const fn private_by_default() -> Self {
        Self {
            capture_enabled: true,
            user_persistence_enabled: false,
            cross_repository_reuse_enabled: false,
            telemetry_enabled: false,
            automatic_proposals_enabled: true,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ReplaySummary {
    pub reproduced: bool,
    pub resolved: bool,
    pub regression_count: u32,
    pub evidence_digests: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LearningReviewItem {
    pub id: String,
    pub revision: u64,
    pub state: LearningReviewState,
    pub kind: LearningKind,
    pub title: String,
    pub source_signal_ids: Vec<String>,
    pub evidence_digests: Vec<String>,
    pub root_cause: String,
    pub generalized_rule: String,
    pub scope: String,
    pub confidence_milli: u16,
    pub proposed_solution: String,
    pub non_applicable_contexts: Vec<String>,
    pub replay: Option<ReplaySummary>,
    pub conflicts_with: BTreeSet<String>,
    pub active_version: Option<String>,
    pub previous_version: Option<String>,
    pub created_at_unix_ms: i64,
    pub updated_at_unix_ms: i64,
}

impl LearningReviewItem {
    fn validate(&self) -> Result<(), LearningReviewError> {
        require_text(&self.id, "learning id cannot be empty")?;
        require_text(&self.title, "learning title cannot be empty")?;
        require_text(&self.generalized_rule, "generalized rule cannot be empty")?;
        require_text(&self.scope, "learning scope cannot be empty")?;
        if self.revision == 0 {
            return Err(LearningReviewError::Validation(
                "learning revision must be positive",
            ));
        }
        if self.confidence_milli > 1_000 {
            return Err(LearningReviewError::Validation(
                "confidence must be between 0 and 1000",
            ));
        }
        reject_sensitive(&self.title)?;
        reject_sensitive(&self.root_cause)?;
        reject_sensitive(&self.generalized_rule)?;
        reject_sensitive(&self.proposed_solution)?;
        for digest in self.evidence_digests.iter().chain(
            self.replay
                .iter()
                .flat_map(|summary| summary.evidence_digests.iter()),
        ) {
            validate_digest(digest)?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AuditEvent {
    pub sequence: u64,
    pub item_id: String,
    pub actor: String,
    pub action: String,
    pub recorded_at_unix_ms: i64,
    pub previous_hash: String,
    pub event_hash: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LearningReviewSnapshot {
    pub schema_version: u32,
    pub revision: u64,
    pub privacy: LearningPrivacy,
    pub items: Vec<LearningReviewItem>,
    pub audit_head: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RedactionPreview {
    pub safe: bool,
    pub blocked_fields: Vec<String>,
    pub warnings: Vec<String>,
    pub item_count: usize,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LearningAuditExport {
    pub snapshot: LearningReviewSnapshot,
    pub events: Vec<AuditEvent>,
    pub chain_valid: bool,
    pub redaction: RedactionPreview,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct StoreDocument {
    schema_version: u32,
    revision: u64,
    privacy: LearningPrivacy,
    items: Vec<LearningReviewItem>,
    audit_head: String,
}

impl Default for StoreDocument {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            revision: 0,
            privacy: LearningPrivacy::private_by_default(),
            items: Vec::new(),
            audit_head: ZERO_HASH.to_owned(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct LearningReviewStore {
    root: PathBuf,
}

impl LearningReviewStore {
    #[must_use]
    pub fn for_repository(repo: &Path) -> Self {
        Self {
            root: repo.join(".medusa/learning-review"),
        }
    }

    pub fn snapshot(&self) -> Result<LearningReviewSnapshot, LearningReviewError> {
        let document = self.load()?;
        Ok(snapshot(document))
    }

    pub fn upsert(
        &self,
        item: LearningReviewItem,
        expected_revision: u64,
        actor: &str,
    ) -> Result<LearningReviewSnapshot, LearningReviewError> {
        item.validate()?;
        let mut document = self.load()?;
        ensure_revision(document.revision, expected_revision)?;
        match document
            .items
            .iter_mut()
            .find(|candidate| candidate.id == item.id)
        {
            Some(existing) => {
                if item.revision <= existing.revision {
                    return Err(LearningReviewError::Validation(
                        "item revision must increase",
                    ));
                }
                *existing = item.clone();
            }
            None => document.items.push(item.clone()),
        }
        document.items.sort_by(|left, right| left.id.cmp(&right.id));
        self.commit(document, &item.id, actor, "upsert")
    }

    pub fn transition(
        &self,
        item_id: &str,
        target: LearningReviewState,
        expected_revision: u64,
        actor: &str,
        now_unix_ms: i64,
    ) -> Result<LearningReviewSnapshot, LearningReviewError> {
        let mut document = self.load()?;
        ensure_revision(document.revision, expected_revision)?;
        let item = document
            .items
            .iter_mut()
            .find(|candidate| candidate.id == item_id)
            .ok_or(LearningReviewError::NotFound)?;
        authorize_transition(item, target)?;
        item.previous_version = item.active_version.clone();
        if target == LearningReviewState::Active {
            let replay = item.replay.as_ref().ok_or(LearningReviewError::Validation(
                "activation requires deterministic replay evidence",
            ))?;
            if !replay.reproduced || !replay.resolved || replay.regression_count > 0 {
                return Err(LearningReviewError::Validation(
                    "activation requires reproduced, resolved, regression-free replay",
                ));
            }
            if !item.conflicts_with.is_empty() {
                item.state = LearningReviewState::Conflict;
                return Err(LearningReviewError::Validation(
                    "conflicting learning requires explicit resolution",
                ));
            }
            item.active_version = Some(format!("{}-r{}", item.id, item.revision));
        }
        if target == LearningReviewState::RolledBack {
            item.active_version = item.previous_version.clone();
        }
        if target == LearningReviewState::Deleted {
            item.title = "Deleted learning".to_owned();
            item.root_cause.clear();
            item.generalized_rule = "deleted".to_owned();
            item.proposed_solution.clear();
            item.source_signal_ids.clear();
            item.non_applicable_contexts.clear();
        }
        item.state = target;
        item.revision = item.revision.saturating_add(1);
        item.updated_at_unix_ms = now_unix_ms;
        self.commit(document, item_id, actor, &format!("transition:{target:?}"))
    }

    pub fn update_privacy(
        &self,
        privacy: LearningPrivacy,
        expected_revision: u64,
        actor: &str,
    ) -> Result<LearningReviewSnapshot, LearningReviewError> {
        let mut document = self.load()?;
        ensure_revision(document.revision, expected_revision)?;
        document.privacy = privacy;
        self.commit(document, "privacy", actor, "privacy:update")
    }

    pub fn redaction_preview(&self) -> Result<RedactionPreview, LearningReviewError> {
        let document = self.load()?;
        Ok(redaction_preview(&document))
    }

    pub fn export(&self) -> Result<LearningAuditExport, LearningReviewError> {
        let document = self.load()?;
        let redaction = redaction_preview(&document);
        if !redaction.safe {
            return Err(LearningReviewError::SensitiveExportBlocked(
                redaction.blocked_fields.clone(),
            ));
        }
        let events = self.read_events()?;
        let chain_valid = verify_chain(&events, &document.audit_head);
        if !chain_valid {
            return Err(LearningReviewError::AuditChainInvalid);
        }
        Ok(LearningAuditExport {
            snapshot: snapshot(document),
            events,
            chain_valid,
            redaction,
        })
    }

    fn commit(
        &self,
        mut document: StoreDocument,
        item_id: &str,
        actor: &str,
        action: &str,
    ) -> Result<LearningReviewSnapshot, LearningReviewError> {
        require_text(actor, "audit actor cannot be empty")?;
        document.revision = document.revision.saturating_add(1);
        let sequence = self.read_events()?.len() as u64 + 1;
        let event = signed_event(
            sequence,
            item_id,
            actor,
            action,
            document.audit_head.clone(),
        );
        document.audit_head = event.event_hash.clone();
        self.write(&document)?;
        self.append_event(&event)?;
        Ok(snapshot(document))
    }

    fn load(&self) -> Result<StoreDocument, LearningReviewError> {
        let path = self.root.join("state.json");
        if !path.exists() {
            return Ok(StoreDocument::default());
        }
        let document: StoreDocument = serde_json::from_slice(&fs::read(path)?)?;
        if document.schema_version != SCHEMA_VERSION {
            return Err(LearningReviewError::UnsupportedSchema(
                document.schema_version,
            ));
        }
        for item in &document.items {
            item.validate()?;
        }
        Ok(document)
    }

    fn write(&self, document: &StoreDocument) -> Result<(), LearningReviewError> {
        fs::create_dir_all(&self.root)?;
        let path = self.root.join("state.json");
        let temporary = self.root.join(format!("state.tmp-{}", std::process::id()));
        fs::write(&temporary, serde_json::to_vec_pretty(document)?)?;
        if path.exists() {
            fs::remove_file(&path)?;
        }
        fs::rename(temporary, path)?;
        Ok(())
    }

    fn append_event(&self, event: &AuditEvent) -> Result<(), LearningReviewError> {
        fs::create_dir_all(&self.root)?;
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.root.join("audit.jsonl"))?;
        serde_json::to_writer(&mut file, event)?;
        file.write_all(b"\n")?;
        file.sync_data()?;
        Ok(())
    }

    fn read_events(&self) -> Result<Vec<AuditEvent>, LearningReviewError> {
        let path = self.root.join("audit.jsonl");
        if !path.exists() {
            return Ok(Vec::new());
        }
        fs::read_to_string(path)?
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| serde_json::from_str(line).map_err(Into::into))
            .collect()
    }
}

fn snapshot(document: StoreDocument) -> LearningReviewSnapshot {
    LearningReviewSnapshot {
        schema_version: document.schema_version,
        revision: document.revision,
        privacy: document.privacy,
        items: document.items,
        audit_head: document.audit_head,
    }
}

fn authorize_transition(
    item: &LearningReviewItem,
    target: LearningReviewState,
) -> Result<(), LearningReviewError> {
    let allowed = matches!(
        (item.state, target),
        (LearningReviewState::Proposed, LearningReviewState::Deferred)
            | (LearningReviewState::Proposed, LearningReviewState::Approved)
            | (LearningReviewState::Proposed, LearningReviewState::Rejected)
            | (LearningReviewState::Deferred, LearningReviewState::Approved)
            | (LearningReviewState::Deferred, LearningReviewState::Rejected)
            | (
                LearningReviewState::Approved,
                LearningReviewState::Validated
            )
            | (LearningReviewState::Validated, LearningReviewState::Active)
            | (LearningReviewState::Active, LearningReviewState::Suspended)
            | (LearningReviewState::Active, LearningReviewState::RolledBack)
            | (LearningReviewState::Suspended, LearningReviewState::Active)
            | (
                LearningReviewState::Suspended,
                LearningReviewState::RolledBack
            )
            | (_, LearningReviewState::Deleted)
    );
    if allowed {
        Ok(())
    } else {
        Err(LearningReviewError::InvalidTransition {
            from: item.state,
            to: target,
        })
    }
}

fn redaction_preview(document: &StoreDocument) -> RedactionPreview {
    let mut blocked_fields = Vec::new();
    for item in &document.items {
        for (name, value) in [
            ("title", item.title.as_str()),
            ("root_cause", item.root_cause.as_str()),
            ("generalized_rule", item.generalized_rule.as_str()),
            ("proposed_solution", item.proposed_solution.as_str()),
        ] {
            if sensitive(value) {
                blocked_fields.push(format!("{}.{}", item.id, name));
            }
        }
    }
    blocked_fields.sort();
    blocked_fields.dedup();
    RedactionPreview {
        safe: blocked_fields.is_empty(),
        blocked_fields,
        warnings: vec![
            "Exports contain generalized rules and evidence digests, never raw microphone transcripts or image bytes."
                .to_owned(),
            "Review the destination before sharing repository-scoped learning.".to_owned(),
        ],
        item_count: document.items.len(),
    }
}

fn signed_event(
    sequence: u64,
    item_id: &str,
    actor: &str,
    action: &str,
    previous_hash: String,
) -> AuditEvent {
    let recorded_at_unix_ms = now_unix_ms();
    let payload =
        format!("{sequence}\n{item_id}\n{actor}\n{action}\n{recorded_at_unix_ms}\n{previous_hash}");
    AuditEvent {
        sequence,
        item_id: item_id.to_owned(),
        actor: actor.to_owned(),
        action: action.to_owned(),
        recorded_at_unix_ms,
        previous_hash,
        event_hash: hex::encode(Sha256::digest(payload.as_bytes())),
    }
}

fn verify_chain(events: &[AuditEvent], expected_head: &str) -> bool {
    let mut previous = ZERO_HASH.to_owned();
    for event in events {
        if event.previous_hash != previous {
            return false;
        }
        let payload = format!(
            "{}\n{}\n{}\n{}\n{}\n{}",
            event.sequence,
            event.item_id,
            event.actor,
            event.action,
            event.recorded_at_unix_ms,
            event.previous_hash
        );
        if event.event_hash != hex::encode(Sha256::digest(payload.as_bytes())) {
            return false;
        }
        previous = event.event_hash.clone();
    }
    previous == expected_head
}

fn now_unix_ms() -> i64 {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    i64::try_from(millis).unwrap_or(i64::MAX)
}

fn ensure_revision(actual: u64, expected: u64) -> Result<(), LearningReviewError> {
    if actual == expected {
        Ok(())
    } else {
        Err(LearningReviewError::Conflict { expected, actual })
    }
}

fn require_text(value: &str, message: &'static str) -> Result<(), LearningReviewError> {
    if value.trim().is_empty() {
        Err(LearningReviewError::Validation(message))
    } else {
        Ok(())
    }
}

fn validate_digest(value: &str) -> Result<(), LearningReviewError> {
    if value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        Ok(())
    } else {
        Err(LearningReviewError::Validation(
            "evidence references must be SHA-256 digests",
        ))
    }
}

fn reject_sensitive(value: &str) -> Result<(), LearningReviewError> {
    if sensitive(value) {
        Err(LearningReviewError::Validation(
            "learning records cannot retain secrets, credentials, microphone transcripts, or image payloads",
        ))
    } else {
        Ok(())
    }
}

fn sensitive(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    [
        "-----begin private key-----",
        "seed phrase",
        "recovery phrase",
        "password=",
        "token=",
        "bearer ",
        "sk-",
        "ghp_",
        "github_pat_",
        "data:image/",
        "microphone transcript:",
        "audio transcript:",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
}

#[derive(Debug)]
pub enum LearningReviewError {
    Io(io::Error),
    Json(serde_json::Error),
    Validation(&'static str),
    Conflict {
        expected: u64,
        actual: u64,
    },
    NotFound,
    InvalidTransition {
        from: LearningReviewState,
        to: LearningReviewState,
    },
    SensitiveExportBlocked(Vec<String>),
    AuditChainInvalid,
    UnsupportedSchema(u32),
    Canonical(String),
}

impl std::fmt::Display for LearningReviewError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "learning review I/O failed: {error}"),
            Self::Json(error) => write!(formatter, "learning review JSON failed: {error}"),
            Self::Validation(message) => formatter.write_str(message),
            Self::Conflict { expected, actual } => write!(
                formatter,
                "learning review revision conflict: expected {expected}, actual {actual}"
            ),
            Self::NotFound => formatter.write_str("learning review item was not found"),
            Self::InvalidTransition { from, to } => {
                write!(
                    formatter,
                    "invalid learning lifecycle transition: {from:?} -> {to:?}"
                )
            }
            Self::SensitiveExportBlocked(fields) => write!(
                formatter,
                "learning export blocked because sensitive content may be present in {}",
                fields.join(", ")
            ),
            Self::AuditChainInvalid => formatter.write_str("learning audit chain is invalid"),
            Self::UnsupportedSchema(version) => {
                write!(
                    formatter,
                    "unsupported learning review schema version {version}"
                )
            }
            Self::Canonical(message) => formatter.write_str(message),
        }
    }
}

impl std::error::Error for LearningReviewError {}

impl From<io::Error> for LearningReviewError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<serde_json::Error> for LearningReviewError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item() -> LearningReviewItem {
        LearningReviewItem {
            id: "lesson-1".to_owned(),
            revision: 1,
            state: LearningReviewState::Proposed,
            kind: LearningKind::Skill,
            title: "Complete repository-wide test plans".to_owned(),
            source_signal_ids: vec!["signal-1".to_owned()],
            evidence_digests: vec!["a".repeat(64)],
            root_cause: "authoritative sources were inventoried too late".to_owned(),
            generalized_rule: "inventory authoritative sources before claiming completeness"
                .to_owned(),
            scope: "repository".to_owned(),
            confidence_milli: 900,
            proposed_solution: "add a completeness workflow gate".to_owned(),
            non_applicable_contexts: vec!["bounded sample".to_owned()],
            replay: Some(ReplaySummary {
                reproduced: true,
                resolved: true,
                regression_count: 0,
                evidence_digests: vec!["b".repeat(64)],
            }),
            conflicts_with: BTreeSet::new(),
            active_version: None,
            previous_version: None,
            created_at_unix_ms: 1,
            updated_at_unix_ms: 1,
        }
    }

    #[test]
    fn lifecycle_is_persistent_and_audit_chain_is_valid() {
        let repo = tempfile::tempdir().expect("repo");
        let store = LearningReviewStore::for_repository(repo.path());
        let mut snapshot = store.upsert(item(), 0, "test").expect("upsert");
        snapshot = store
            .transition(
                "lesson-1",
                LearningReviewState::Approved,
                snapshot.revision,
                "test",
                2,
            )
            .expect("approve");
        snapshot = store
            .transition(
                "lesson-1",
                LearningReviewState::Validated,
                snapshot.revision,
                "test",
                3,
            )
            .expect("validate");
        snapshot = store
            .transition(
                "lesson-1",
                LearningReviewState::Active,
                snapshot.revision,
                "test",
                4,
            )
            .expect("activate");
        assert_eq!(snapshot.items[0].state, LearningReviewState::Active);
        assert!(snapshot.items[0].active_version.is_some());
        let export = store.export().expect("export");
        assert!(export.chain_valid);
        assert_eq!(export.events.len(), 4);
    }

    #[test]
    fn secrets_and_image_payloads_fail_closed() {
        let repo = tempfile::tempdir().expect("repo");
        let store = LearningReviewStore::for_repository(repo.path());
        let mut unsafe_item = item();
        unsafe_item.generalized_rule = "use token=secret".to_owned();
        assert!(store.upsert(unsafe_item, 0, "test").is_err());
        let mut image_item = item();
        image_item.proposed_solution = "retain data:image/png;base64,abc".to_owned();
        assert!(store.upsert(image_item, 0, "test").is_err());
    }

    #[test]
    fn stale_clients_and_invalid_activation_fail_closed() {
        let repo = tempfile::tempdir().expect("repo");
        let store = LearningReviewStore::for_repository(repo.path());
        let snapshot = store.upsert(item(), 0, "test").expect("upsert");
        assert!(
            store
                .update_privacy(LearningPrivacy::private_by_default(), 0, "stale")
                .is_err()
        );
        assert!(
            store
                .transition(
                    "lesson-1",
                    LearningReviewState::Active,
                    snapshot.revision,
                    "test",
                    2,
                )
                .is_err()
        );
    }
}
