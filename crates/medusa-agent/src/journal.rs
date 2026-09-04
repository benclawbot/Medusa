use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard, OnceLock, Weak},
    time::SystemTime,
};

use medusa_core::{CorrelationId, ErrorCategory, ErrorCode, MedusaError, MedusaResult, SessionId};
use medusa_protocol::{
    Actor, EventEnvelope, EventPayload, SessionAction, SessionActionKind, SessionActionLifecycle,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use time::OffsetDateTime;

use super::{AgentSession, fallback_storage_root};
use crate::evidence::{verify_chain, verify_chain_incremental};

const JOURNAL_MAGIC: &[u8; 8] = b"MDJNL002";
const FRAME_HEADER_BYTES: usize = std::mem::size_of::<u32>() + 32;
const MAX_FRAME_BYTES: usize = 32 * 1024 * 1024;

static JOURNAL_LOCKS: OnceLock<Mutex<BTreeMap<String, Weak<Mutex<()>>>>> = OnceLock::new();
static JOURNAL_CACHE: OnceLock<Mutex<BTreeMap<PathBuf, CachedJournal>>> = OnceLock::new();

#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AppendDisposition {
    Appended,
    Replayed,
}

pub(crate) struct LoadOutcome {
    pub session: AgentSession,
    pub repair_snapshot: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "record", rename_all = "snake_case")]
enum JournalRecord {
    Event {
        event: Box<EventEnvelope>,
    },
    Snapshot {
        cursor: u64,
        final_event_checksum: Option<String>,
        session: Box<AgentSession>,
    },
}

struct JournalState {
    events: Vec<EventEnvelope>,
    committed_snapshot: Option<AgentSession>,
}

struct CachedJournal {
    length: u64,
    modified: Option<SystemTime>,
    state: Arc<JournalState>,
}

pub(crate) fn append_payload_committed(
    session: &mut AgentSession,
    actor: Actor,
    payload: EventPayload,
) -> MedusaResult<EventEnvelope> {
    let lock = session_lock(&session.repo, &session.id);
    let _guard = lock_mutex(&lock);
    verify_chain_incremental(&session.id.to_string(), &session.events)?;
    ensure_initialized(session)?;
    let path = journal_path(&session.repo, &session.id)?;
    let state = read_journal(&path, &session.id, true, true)?;
    merge_committed_events(session, &state.events)?;

    if let EventPayload::SessionActionAccepted { action } = &payload
        && let Some(existing) = replayed_action_admission(&session.events, action)?
    {
        return Ok(existing);
    }
    let payload = normalize_action_admission(&session.events, payload)?;
    let event = EventEnvelope::new(
        u64::try_from(session.events.len())
            .unwrap_or(u64::MAX)
            .saturating_add(1),
        session.id.clone(),
        actor,
        CorrelationId::new(),
        payload,
        session.events.last().map(|event| event.checksum.clone()),
        OffsetDateTime::now_utc(),
    )?;
    let mut committed = session.clone();
    committed.events.push(event.clone());
    let records = [
        JournalRecord::Event {
            event: Box::new(event.clone()),
        },
        snapshot_record_owned(committed),
    ];
    append_records(&path, &records)?;
    session.events.push(event.clone());
    Ok(event)
}

fn replayed_action_admission(
    events: &[EventEnvelope],
    action: &SessionAction,
) -> MedusaResult<Option<EventEnvelope>> {
    for event in events {
        let existing = match &event.payload {
            EventPayload::SessionActionAccepted { action }
            | EventPayload::SessionActionRejected { action, .. } => action,
            _ => continue,
        };
        if existing.action_id == action.action_id
            || existing.idempotency_key == action.idempotency_key
        {
            if existing == action {
                return Ok(Some(event.clone()));
            }
            return Err(persistence_error(
                "session action idempotency identity was reused with conflicting content",
            ));
        }
    }
    Ok(None)
}

fn normalize_action_admission(
    events: &[EventEnvelope],
    payload: EventPayload,
) -> MedusaResult<EventPayload> {
    let EventPayload::SessionActionAccepted { action } = payload else {
        return Ok(payload);
    };
    action.validate().map_err(persistence_error)?;
    let authoritative_revision = u64::try_from(events.len()).unwrap_or(u64::MAX);
    if action.expected_session_revision != authoritative_revision {
        return Ok(EventPayload::SessionActionRejected {
            action,
            authoritative_revision,
            reason: "stale_revision".to_owned(),
        });
    }
    if action.kind != SessionActionKind::ReplaceFollowUp {
        return Ok(EventPayload::SessionActionAccepted { action });
    }
    let Some(replaces_action_id) = replacement_target_id(&action) else {
        return Ok(EventPayload::SessionActionRejected {
            action,
            authoritative_revision,
            reason: "replacement_target_missing".to_owned(),
        });
    };
    let replaceable = action_state(events, replaces_action_id)?.is_some_and(|(kind, lifecycle)| {
        matches!(
            kind,
            SessionActionKind::FollowUp | SessionActionKind::ReplaceFollowUp
        ) && lifecycle == SessionActionLifecycle::Queued
    });
    if !replaceable {
        return Ok(EventPayload::SessionActionRejected {
            action,
            authoritative_revision,
            reason: "replacement_target_not_queued_follow_up".to_owned(),
        });
    }
    Ok(EventPayload::SessionActionAccepted { action })
}

fn replacement_target_id(action: &SessionAction) -> Option<&str> {
    (action.kind == SessionActionKind::ReplaceFollowUp)
        .then(|| action.payload.get("replaces_action_id"))
        .flatten()
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn action_state(
    events: &[EventEnvelope],
    action_id: &str,
) -> MedusaResult<Option<(SessionActionKind, SessionActionLifecycle)>> {
    let mut state = None;
    for event in events {
        match &event.payload {
            EventPayload::SessionActionAccepted { action } if action.action_id == action_id => {
                state = Some((action.kind, SessionActionLifecycle::Queued));
            }
            EventPayload::SessionActionRejected { action, .. } if action.action_id == action_id => {
                state = Some((action.kind, SessionActionLifecycle::Failed));
            }
            EventPayload::SessionActionLifecycleChanged {
                action_id: changed,
                from,
                to,
                ..
            } if changed == action_id => {
                let Some((kind, lifecycle)) = state else {
                    return Err(persistence_error(
                        "session action lifecycle has no prior admission",
                    ));
                };
                if lifecycle != *from || !from.can_transition_to(*to) {
                    return Err(persistence_error(
                        "session action lifecycle is invalid while evaluating admission",
                    ));
                }
                state = Some((kind, *to));
            }
            EventPayload::SessionActionAccepted { action }
                if replacement_target_id(action) == Some(action_id) =>
            {
                let Some((kind, lifecycle)) = state else {
                    return Err(persistence_error(
                        "replacement action targets a missing prior admission",
                    ));
                };
                if lifecycle != SessionActionLifecycle::Queued
                    || !matches!(
                        kind,
                        SessionActionKind::FollowUp | SessionActionKind::ReplaceFollowUp
                    )
                {
                    return Err(persistence_error(
                        "replacement action supersedes a non-queued follow-up",
                    ));
                }
                state = Some((kind, SessionActionLifecycle::Cancelled));
            }
            _ => {}
        }
    }
    Ok(state)
}

fn merge_committed_events(
    session: &mut AgentSession,
    committed_events: &[EventEnvelope],
) -> MedusaResult<()> {
    if session.events.len() > committed_events.len()
        || session.events.as_slice() != &committed_events[..session.events.len()]
    {
        return Err(persistence_error(
            "materialized session diverges from the committed journal",
        ));
    }
    if committed_events.len() > session.events.len() {
        session
            .events
            .extend_from_slice(&committed_events[session.events.len()..]);
    }
    Ok(())
}

#[cfg(test)]
pub(crate) fn append_event(
    session: &AgentSession,
    event: &EventEnvelope,
) -> MedusaResult<AppendDisposition> {
    let lock = session_lock(&session.repo, &session.id);
    let _guard = lock_mutex(&lock);
    verify_chain_incremental(&session.id.to_string(), &session.events)?;
    event.validate()?;
    if event.session_id != session.id {
        return Err(persistence_error(
            "journal event belongs to another session",
        ));
    }

    if let Some(existing) = session
        .events
        .iter()
        .find(|existing| existing.event_id == event.event_id)
    {
        if existing != event {
            return Err(persistence_error(format!(
                "event id {} was reused with conflicting content",
                event.event_id
            )));
        }
        ensure_initialized(session)?;
        let path = journal_path(&session.repo, &session.id)?;
        let state = read_journal(&path, &session.id, true, false)?;
        let index = usize::try_from(event.sequence.saturating_sub(1))
            .map_err(|_| persistence_error("event sequence is unsupported on this platform"))?;
        if state.events.get(index) == Some(event) {
            return Ok(AppendDisposition::Replayed);
        }
        return Err(persistence_error(
            "materialized session contains an event missing from its journal",
        ));
    }

    let expected_sequence = u64::try_from(session.events.len())
        .unwrap_or(u64::MAX)
        .saturating_add(1);
    if event.sequence != expected_sequence {
        return Err(persistence_error(format!(
            "event sequence {} does not follow materialized cursor {}",
            event.sequence,
            session.events.len()
        )));
    }
    let expected_previous = session
        .events
        .last()
        .map(|previous| previous.checksum.as_str());
    if event.previous_hash.as_deref() != expected_previous {
        return Err(persistence_error(
            "event previous hash does not match the materialized session",
        ));
    }

    ensure_initialized(session)?;
    let path = journal_path(&session.repo, &session.id)?;
    let state = read_journal(&path, &session.id, true, false)?;
    if state.events != session.events {
        return Err(persistence_error(
            "journal event prefix does not match the materialized session",
        ));
    }
    append_record(
        &path,
        &JournalRecord::Event {
            event: Box::new(event.clone()),
        },
    )?;
    Ok(AppendDisposition::Appended)
}

#[cfg(test)]
pub(crate) fn commit_snapshot(session: &AgentSession) -> MedusaResult<AgentSession> {
    let lock = session_lock(&session.repo, &session.id);
    let _guard = lock_mutex(&lock);
    commit_snapshot_locked(session)
}

pub(crate) fn commit_snapshot_with<F>(
    session: &AgentSession,
    after_commit: F,
) -> MedusaResult<AgentSession>
where
    F: FnOnce(&AgentSession) -> MedusaResult<()>,
{
    let lock = session_lock(&session.repo, &session.id);
    let _guard = lock_mutex(&lock);
    let committed = commit_snapshot_locked(session)?;
    after_commit(&committed)?;
    Ok(committed)
}

fn commit_snapshot_locked(session: &AgentSession) -> MedusaResult<AgentSession> {
    verify_chain_incremental(&session.id.to_string(), &session.events)?;
    ensure_initialized(session)?;
    let path = journal_path(&session.repo, &session.id)?;
    let state = read_journal(&path, &session.id, true, false)?;
    let mut merged = session.clone();

    if state.events != merged.events {
        if let Some(committed) = state.committed_snapshot.as_ref() {
            merge_committed_events(&mut merged, &committed.events)?;
        }
    }

    if state.events != merged.events {
        let recovered = read_journal(&path, &session.id, true, true)?;
        merged = session.clone();
        merge_committed_events(&mut merged, &recovered.events)?;
        if recovered.events != merged.events {
            return Err(persistence_error(
                "cannot commit a session whose events diverge from the journal",
            ));
        }
    }

    append_record(&path, &snapshot_record(&merged))?;
    Ok(merged)
}

pub(crate) fn load_or_migrate(
    repo: &Path,
    session_id: &SessionId,
    snapshot: Option<AgentSession>,
) -> MedusaResult<LoadOutcome> {
    let lock = session_lock(repo, session_id);
    let _guard = lock_mutex(&lock);
    if let Some(snapshot) = &snapshot {
        validate_snapshot_identity(repo, session_id, snapshot)?;
        verify_chain(&snapshot.events)?;
    }

    let Some(path) = existing_journal_path(repo, session_id) else {
        let snapshot = snapshot.ok_or_else(|| {
            persistence_error(format!(
                "session {session_id} has neither a materialized snapshot nor a journal"
            ))
        })?;
        initialize_journal(&snapshot)?;
        return Ok(LoadOutcome {
            session: snapshot,
            repair_snapshot: false,
        });
    };

    let state = read_journal(&path, session_id, true, true)?;
    let Some(committed) = state.committed_snapshot else {
        let snapshot = snapshot.ok_or_else(|| {
            persistence_error(format!(
                "session {session_id} journal has no committed snapshot"
            ))
        })?;
        rewrite_journal(&path, &snapshot)?;
        return Ok(LoadOutcome {
            session: snapshot,
            repair_snapshot: false,
        });
    };
    validate_snapshot_identity(repo, session_id, &committed)?;

    let repair_snapshot = match snapshot {
        Some(snapshot) => {
            if snapshot.events.len() > committed.events.len()
                || snapshot.events.as_slice() != &committed.events[..snapshot.events.len()]
            {
                return Err(persistence_error(
                    "materialized snapshot diverges from the committed journal",
                ));
            }
            serde_json::to_vec(&snapshot)? != serde_json::to_vec(&committed)?
        }
        None => true,
    };
    Ok(LoadOutcome {
        session: committed,
        repair_snapshot,
    })
}

pub(crate) fn replay_from_cursor(
    repo: &Path,
    session_id: &SessionId,
    cursor: u64,
) -> MedusaResult<Vec<EventEnvelope>> {
    let lock = session_lock(repo, session_id);
    let _guard = lock_mutex(&lock);
    let path = journal_path(repo, session_id)?;
    let state = read_journal_cached(&path, session_id)?;
    let committed = state.committed_snapshot.as_ref().ok_or_else(|| {
        persistence_error(format!(
            "session {session_id} journal has no committed snapshot"
        ))
    })?;
    let cursor = usize::try_from(cursor)
        .map_err(|_| persistence_error("replay cursor is unsupported on this platform"))?;
    if cursor > committed.events.len() {
        return Err(persistence_error(format!(
            "replay cursor {cursor} exceeds committed event count {}",
            committed.events.len()
        )));
    }
    Ok(committed.events[cursor..].to_vec())
}

pub(crate) fn discover_session_ids(repo: &Path) -> MedusaResult<Vec<SessionId>> {
    let mut ids = BTreeSet::new();
    collect_journal_ids(&primary_journal_root(repo), &mut ids)?;
    collect_journal_ids(&fallback_journal_root(repo), &mut ids)?;
    Ok(ids.into_iter().collect())
}

fn ensure_initialized(session: &AgentSession) -> MedusaResult<()> {
    if existing_journal_path(&session.repo, &session.id).is_some() {
        return Ok(());
    }
    initialize_journal(session)
}

fn initialize_journal(session: &AgentSession) -> MedusaResult<()> {
    verify_chain_incremental(&session.id.to_string(), &session.events)?;
    let primary = primary_journal_path(&session.repo, &session.id);
    match write_journal(&primary, session) {
        Ok(()) => Ok(()),
        Err(_) => {
            let _ = fs::remove_file(&primary);
            write_journal(&fallback_journal_path(&session.repo, &session.id), session)
        }
    }
}

fn rewrite_journal(path: &Path, session: &AgentSession) -> MedusaResult<()> {
    verify_chain_incremental(&session.id.to_string(), &session.events)?;
    write_journal(path, session)
}

fn write_journal(path: &Path, session: &AgentSession) -> MedusaResult<()> {
    create_parent(path)?;
    let temporary = path.with_extension("events.tmp");
    let mut file = File::create(&temporary)?;
    file.write_all(JOURNAL_MAGIC)?;
    for event in &session.events {
        write_record(
            &mut file,
            &JournalRecord::Event {
                event: Box::new(event.clone()),
            },
        )?;
    }
    write_record(&mut file, &snapshot_record(session))?;
    file.sync_all()?;
    fs::rename(temporary, path)?;
    invalidate_journal_cache(path);
    Ok(())
}

fn snapshot_record(session: &AgentSession) -> JournalRecord {
    snapshot_record_owned(session.clone())
}

fn snapshot_record_owned(session: AgentSession) -> JournalRecord {
    let cursor = u64::try_from(session.events.len()).unwrap_or(u64::MAX);
    let final_event_checksum = session.events.last().map(|event| event.checksum.clone());
    JournalRecord::Snapshot {
        cursor,
        final_event_checksum,
        session: Box::new(session),
    }
}

fn append_record(path: &Path, record: &JournalRecord) -> MedusaResult<()> {
    append_records(path, std::slice::from_ref(record))
}

fn append_records(path: &Path, records: &[JournalRecord]) -> MedusaResult<()> {
    create_parent(path)?;
    let mut file = OpenOptions::new().append(true).open(path)?;
    for record in records {
        write_record(&mut file, record)?;
    }
    file.sync_data()?;
    invalidate_journal_cache(path);
    Ok(())
}

fn read_journal_cached(path: &Path, session_id: &SessionId) -> MedusaResult<Arc<JournalState>> {
    let metadata = fs::metadata(path)?;
    let length = metadata.len();
    let modified = metadata.modified().ok();
    let cache = JOURNAL_CACHE.get_or_init(|| Mutex::new(BTreeMap::new()));
    if let Some(cached) = cache
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(path)
        .filter(|cached| cached.length == length && cached.modified == modified)
    {
        return Ok(Arc::clone(&cached.state));
    }

    let state = Arc::new(read_journal(path, session_id, true, true)?);
    let final_metadata = fs::metadata(path)?;
    cache
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(
            path.to_path_buf(),
            CachedJournal {
                length: final_metadata.len(),
                modified: final_metadata.modified().ok(),
                state: Arc::clone(&state),
            },
        );
    Ok(state)
}

fn invalidate_journal_cache(path: &Path) {
    if let Some(cache) = JOURNAL_CACHE.get() {
        cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(path);
    }
}

fn write_record(file: &mut File, record: &JournalRecord) -> MedusaResult<()> {
    let payload = serde_json::to_vec(record)?;
    if payload.is_empty() || payload.len() > MAX_FRAME_BYTES {
        return Err(persistence_error(
            "journal frame exceeds the supported size",
        ));
    }
    let length = u32::try_from(payload.len())
        .map_err(|_| persistence_error("journal frame length is unsupported"))?;
    let checksum = Sha256::digest(&payload);
    file.write_all(&length.to_be_bytes())?;
    file.write_all(&checksum)?;
    file.write_all(&payload)?;
    Ok(())
}

fn read_journal(
    path: &Path,
    session_id: &SessionId,
    recover_torn_tail: bool,
    discard_uncommitted_tail: bool,
) -> MedusaResult<JournalState> {
    let mut bytes = Vec::new();
    File::open(path)?.read_to_end(&mut bytes)?;
    if bytes.len() < JOURNAL_MAGIC.len() || &bytes[..JOURNAL_MAGIC.len()] != JOURNAL_MAGIC {
        return Err(persistence_error(
            "journal header is missing or unsupported",
        ));
    }

    let mut offset = JOURNAL_MAGIC.len();
    let mut valid_end = offset;
    let mut events = Vec::new();
    let mut event_ids = BTreeSet::new();
    let mut committed_snapshot = None;
    let mut last_snapshot_end = JOURNAL_MAGIC.len();
    let mut torn_tail = false;

    while offset < bytes.len() {
        if bytes.len() - offset < FRAME_HEADER_BYTES {
            torn_tail = true;
            break;
        }
        let length = usize::try_from(u32::from_be_bytes(
            bytes[offset..offset + 4]
                .try_into()
                .map_err(|_| persistence_error("journal frame header is invalid"))?,
        ))
        .map_err(|_| persistence_error("journal frame length is unsupported"))?;
        if length == 0 || length > MAX_FRAME_BYTES {
            return Err(persistence_error("journal frame length is invalid"));
        }
        let checksum_start = offset + 4;
        let payload_start = checksum_start + 32;
        let payload_end = payload_start
            .checked_add(length)
            .ok_or_else(|| persistence_error("journal frame length overflowed"))?;
        if payload_end > bytes.len() {
            torn_tail = true;
            break;
        }
        let expected_checksum = &bytes[checksum_start..payload_start];
        let payload = &bytes[payload_start..payload_end];
        let actual_checksum = Sha256::digest(payload);
        if &actual_checksum[..] != expected_checksum {
            return Err(persistence_error("journal frame checksum mismatch"));
        }
        let record: JournalRecord = serde_json::from_slice(payload)?;
        match record {
            JournalRecord::Event { event } => {
                validate_event(session_id, &events, &event, &mut event_ids)?;
                events.push(*event);
            }
            JournalRecord::Snapshot {
                cursor,
                final_event_checksum,
                session,
            } => {
                validate_committed_snapshot(
                    session_id,
                    &events,
                    cursor,
                    final_event_checksum.as_deref(),
                    &session,
                )?;
                committed_snapshot = Some(*session);
                last_snapshot_end = payload_end;
            }
        }
        offset = payload_end;
        valid_end = offset;
    }

    if torn_tail {
        if !recover_torn_tail {
            return Err(persistence_error("journal has a torn final frame"));
        }
        truncate(path, valid_end)?;
    }
    if discard_uncommitted_tail && committed_snapshot.is_some() && valid_end > last_snapshot_end {
        truncate(path, last_snapshot_end)?;
        events.truncate(
            committed_snapshot
                .as_ref()
                .map_or(0, |session| session.events.len()),
        );
    }

    Ok(JournalState {
        events,
        committed_snapshot,
    })
}

fn validate_event(
    session_id: &SessionId,
    prior_events: &[EventEnvelope],
    event: &EventEnvelope,
    event_ids: &mut BTreeSet<String>,
) -> MedusaResult<()> {
    event.validate()?;
    if &event.session_id != session_id {
        return Err(persistence_error(
            "journal contains an event for another session",
        ));
    }
    let expected_sequence = u64::try_from(prior_events.len())
        .unwrap_or(u64::MAX)
        .saturating_add(1);
    if event.sequence != expected_sequence {
        return Err(persistence_error(format!(
            "journal event sequence {} is not {expected_sequence}",
            event.sequence
        )));
    }
    let expected_previous = prior_events.last().map(|event| event.checksum.as_str());
    if event.previous_hash.as_deref() != expected_previous {
        return Err(persistence_error(
            "journal event previous hash chain is invalid",
        ));
    }
    if !event_ids.insert(event.event_id.to_string()) {
        return Err(persistence_error(format!(
            "journal event id {} appears more than once",
            event.event_id
        )));
    }
    Ok(())
}

fn validate_committed_snapshot(
    session_id: &SessionId,
    events: &[EventEnvelope],
    cursor: u64,
    final_event_checksum: Option<&str>,
    session: &AgentSession,
) -> MedusaResult<()> {
    validate_snapshot_identity(&session.repo, session_id, session)?;
    verify_chain_incremental(&session.id.to_string(), &session.events)?;
    let expected_cursor = u64::try_from(events.len()).unwrap_or(u64::MAX);
    if cursor != expected_cursor || session.events.len() != events.len() {
        return Err(persistence_error(
            "journal snapshot cursor does not match its event prefix",
        ));
    }
    if session.events != events {
        return Err(persistence_error(
            "journal snapshot events do not match preceding event records",
        ));
    }
    let expected_checksum = events.last().map(|event| event.checksum.as_str());
    if final_event_checksum != expected_checksum {
        return Err(persistence_error(
            "journal snapshot checksum does not match its event prefix",
        ));
    }
    Ok(())
}

fn validate_snapshot_identity(
    repo: &Path,
    session_id: &SessionId,
    session: &AgentSession,
) -> MedusaResult<()> {
    if &session.id != session_id {
        return Err(persistence_error(
            "journal snapshot belongs to another session",
        ));
    }
    if normalized_repository_path(&session.repo) != normalized_repository_path(repo) {
        return Err(persistence_error(format!(
            "journal snapshot repository {} does not match {}",
            session.repo.display(),
            repo.display()
        )));
    }
    Ok(())
}

fn normalized_repository_path(path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{rest}"));
    }
    text.strip_prefix(r"\\?\")
        .map_or_else(|| path.to_path_buf(), PathBuf::from)
}

fn truncate(path: &Path, length: usize) -> MedusaResult<()> {
    let length =
        u64::try_from(length).map_err(|_| persistence_error("journal length is unsupported"))?;
    let file = OpenOptions::new().write(true).open(path)?;
    file.set_len(length)?;
    file.sync_data()?;
    Ok(())
}

fn collect_journal_ids(root: &Path, ids: &mut BTreeSet<SessionId>) -> MedusaResult<()> {
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    for entry in entries {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let path = entry.path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("events") {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
            continue;
        };
        if let Ok(id) = SessionId::parse(stem) {
            ids.insert(id);
        }
    }
    Ok(())
}

fn journal_path(repo: &Path, session_id: &SessionId) -> MedusaResult<PathBuf> {
    existing_journal_path(repo, session_id)
        .ok_or_else(|| persistence_error(format!("session {session_id} journal does not exist")))
}

fn existing_journal_path(repo: &Path, session_id: &SessionId) -> Option<PathBuf> {
    let primary = primary_journal_path(repo, session_id);
    if primary.is_file() {
        return Some(primary);
    }
    let fallback = fallback_journal_path(repo, session_id);
    fallback.is_file().then_some(fallback)
}

fn primary_journal_root(repo: &Path) -> PathBuf {
    repo.join(".medusa/journals")
}

fn fallback_journal_root(repo: &Path) -> PathBuf {
    fallback_storage_root(repo, "journals")
}

fn primary_journal_path(repo: &Path, session_id: &SessionId) -> PathBuf {
    primary_journal_root(repo).join(format!("{session_id}.events"))
}

fn fallback_journal_path(repo: &Path, session_id: &SessionId) -> PathBuf {
    fallback_journal_root(repo).join(format!("{session_id}.events"))
}

fn create_parent(path: &Path) -> MedusaResult<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    Ok(())
}

fn session_lock(repo: &Path, session_id: &SessionId) -> Arc<Mutex<()>> {
    let key = format!("{}\0{session_id}", repo.display());
    let registry = JOURNAL_LOCKS.get_or_init(|| Mutex::new(BTreeMap::new()));
    let mut locks = match registry.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    locks.retain(|_, lock| lock.strong_count() > 0);
    if let Some(lock) = locks.get(&key).and_then(Weak::upgrade) {
        return lock;
    }
    let lock = Arc::new(Mutex::new(()));
    locks.insert(key, Arc::downgrade(&lock));
    lock
}

fn lock_mutex(lock: &Mutex<()>) -> MutexGuard<'_, ()> {
    match lock.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

fn persistence_error(message: impl Into<String>) -> MedusaError {
    MedusaError::new(
        ErrorCode::PersistenceFailed,
        ErrorCategory::Persistence,
        message,
    )
}

#[cfg(test)]
mod tests {
    use medusa_core::CorrelationId;
    use medusa_protocol::{
        Actor, EventPayload, SessionAction, SessionActionDeliveryPolicy, SessionActionKind,
        SessionActionWakePolicy,
    };
    use medusa_provider::{Message, MessageBlock, Role};
    use serde_json::json;
    use time::OffsetDateTime;

    use super::*;

    fn session(repo: &Path) -> AgentSession {
        AgentSession {
            id: SessionId::new(),
            objective: "journal test".to_owned(),
            repo: repo.to_path_buf(),
            created_at: OffsetDateTime::UNIX_EPOCH,
            updated_at: OffsetDateTime::UNIX_EPOCH,
            completed: false,
            turn: 0,
            plan: Vec::new(),
            pending_question: None,
            messages: vec![Message {
                role: Role::User,
                content: vec![MessageBlock::Text {
                    text: "journal test".to_owned(),
                }],
            }],
            events: Vec::new(),
            evidence: Vec::new(),
            tool_artifacts: Vec::new(),
            world_model: None,
            approval_grants: Vec::new(),
            approval_receipts: Vec::new(),
            rollback_receipts: Vec::new(),
            codex_thread_id: None,
        }
    }

    fn next_event(session: &AgentSession, payload: EventPayload) -> EventEnvelope {
        EventEnvelope::new(
            u64::try_from(session.events.len())
                .unwrap_or(u64::MAX)
                .saturating_add(1),
            session.id.clone(),
            Actor::Coordinator,
            CorrelationId::new(),
            payload,
            session.events.last().map(|event| event.checksum.clone()),
            OffsetDateTime::UNIX_EPOCH,
        )
        .expect("event")
    }

    fn action(
        session: &AgentSession,
        id: &str,
        expected_session_revision: u64,
        kind: SessionActionKind,
        payload: serde_json::Value,
    ) -> SessionAction {
        SessionAction {
            action_id: format!("action-{id}"),
            idempotency_key: format!("idem-{id}"),
            source: "test".to_owned(),
            target_session_id: session.id.to_string(),
            expected_session_revision,
            kind,
            delivery_policy: if kind == SessionActionKind::Steer {
                SessionActionDeliveryPolicy::NextSafeTurnBoundary
            } else {
                SessionActionDeliveryPolicy::WhenIdle
            },
            wake_policy: SessionActionWakePolicy::OnBoundary,
            payload,
        }
    }

    fn append_and_materialize(session: &mut AgentSession, event: EventEnvelope) {
        assert_eq!(
            append_event(session, &event).expect("append"),
            AppendDisposition::Appended
        );
        session.events.push(event);
    }

    #[test]
    fn committed_snapshot_repairs_stale_json_state() {
        let directory = tempfile::tempdir().expect("repository");
        let mut current = session(directory.path());
        let created = next_event(
            &current,
            EventPayload::SessionCreated {
                objective: current.objective.clone(),
            },
        );
        append_and_materialize(&mut current, created);
        commit_snapshot(&current).expect("first commit");
        let stale = current.clone();

        current.turn = 2;
        current.objective = "updated objective".to_owned();
        let updated = next_event(
            &current,
            EventPayload::GoalUpdated {
                objective: current.objective.clone(),
            },
        );
        append_and_materialize(&mut current, updated);
        commit_snapshot(&current).expect("second commit");

        let outcome = load_or_migrate(directory.path(), &current.id, Some(stale))
            .expect("authoritative load");
        assert!(outcome.repair_snapshot);
        assert_eq!(outcome.session.turn, 2);
        assert_eq!(outcome.session.objective, "updated objective");
        assert_eq!(outcome.session.events.len(), 2);
    }

    #[test]
    fn complete_but_uncommitted_event_tail_is_discarded() {
        let directory = tempfile::tempdir().expect("repository");
        let mut committed = session(directory.path());
        let created = next_event(
            &committed,
            EventPayload::SessionCreated {
                objective: committed.objective.clone(),
            },
        );
        append_and_materialize(&mut committed, created);
        commit_snapshot(&committed).expect("commit");

        let pending = next_event(&committed, EventPayload::SessionResumed);
        assert_eq!(
            append_event(&committed, &pending).expect("write-ahead event"),
            AppendDisposition::Appended
        );

        let outcome = load_or_migrate(directory.path(), &committed.id, Some(committed.clone()))
            .expect("recover");
        assert_eq!(outcome.session.events, committed.events);
        assert!(
            replay_from_cursor(directory.path(), &committed.id, 1)
                .expect("replay")
                .is_empty()
        );
        assert_eq!(
            append_event(&committed, &pending).expect("append after recovery"),
            AppendDisposition::Appended
        );
    }

    #[test]
    fn torn_final_frame_is_truncated_to_the_last_commit() {
        let directory = tempfile::tempdir().expect("repository");
        let mut committed = session(directory.path());
        let created = next_event(
            &committed,
            EventPayload::SessionCreated {
                objective: committed.objective.clone(),
            },
        );
        append_and_materialize(&mut committed, created);
        commit_snapshot(&committed).expect("commit");
        let path = journal_path(directory.path(), &committed.id).expect("journal path");
        let valid_length = fs::metadata(&path).expect("metadata").len();
        OpenOptions::new()
            .append(true)
            .open(&path)
            .expect("journal")
            .write_all(&12_u32.to_be_bytes())
            .expect("partial frame");

        let outcome = load_or_migrate(directory.path(), &committed.id, Some(committed.clone()))
            .expect("recover");
        assert_eq!(outcome.session.events, committed.events);
        assert_eq!(fs::metadata(path).expect("metadata").len(), valid_length);
    }

    #[test]
    fn journal_persistence_benchmark_reports_required_metrics() {
        let directory = tempfile::tempdir().expect("repository");
        let mut current = session(directory.path());
        initialize_journal(&current).expect("initialize journal");
        let path = journal_path(directory.path(), &current.id).expect("journal path");
        let initial_len = fs::metadata(&path).expect("initial metadata").len();

        let snapshot_started = std::time::Instant::now();
        let snapshot_bytes = serde_json::to_vec(&snapshot_record(&current))
            .expect("snapshot serialization")
            .len();
        let snapshot_time_ns = snapshot_started.elapsed().as_nanos();

        let lock = session_lock(&current.repo, &current.id);
        let lock_started = std::time::Instant::now();
        let guard = lock_mutex(&lock);
        let lock_wait_ns = lock_started.elapsed().as_nanos();
        drop(guard);

        let objective = current.objective.clone();
        let critical_started = std::time::Instant::now();
        append_payload_committed(
            &mut current,
            Actor::Coordinator,
            EventPayload::SessionCreated { objective },
        )
        .expect("committed append");
        let critical_path_ns = critical_started.elapsed().as_nanos();

        let bytes = fs::read(&path).expect("journal bytes");
        let mut offset = usize::try_from(initial_len).expect("journal length");
        let mut serialized_bytes = 0_usize;
        let mut records = 0_usize;
        while offset < bytes.len() {
            let length = usize::try_from(u32::from_be_bytes(
                bytes[offset..offset + 4].try_into().expect("frame length"),
            ))
            .expect("supported frame length");
            serialized_bytes += length;
            records += 1;
            offset += FRAME_HEADER_BYTES + length;
        }
        assert_eq!(records, 2, "one event and one snapshot are batched");

        let metrics = json!({
            "journal_writes": 1,
            "file_syncs": 1,
            "records": records,
            "bytes_serialized": serialized_bytes,
            "bytes_copied_lower_bound": serialized_bytes,
            "lock_wait_ns": lock_wait_ns,
            "snapshot_bytes": snapshot_bytes,
            "snapshot_time_ns": snapshot_time_ns,
            "critical_path_ns": critical_path_ns,
        });
        println!("JOURNAL_PERSISTENCE_METRICS={metrics}");
        assert!(serialized_bytes > 0);
        assert!(snapshot_bytes > 0);
        assert_eq!(current.events.len(), 1);
    }

    #[test]
    fn committed_append_batches_event_then_snapshot_in_canonical_order() {
        let directory = tempfile::tempdir().expect("repository");
        let mut current = session(directory.path());
        let objective = current.objective.clone();
        append_payload_committed(
            &mut current,
            Actor::Coordinator,
            EventPayload::SessionCreated { objective },
        )
        .expect("committed append");

        let path = journal_path(directory.path(), &current.id).expect("journal path");
        let bytes = fs::read(path).expect("journal bytes");
        let mut offset = JOURNAL_MAGIC.len();
        let mut records = Vec::new();
        while offset < bytes.len() {
            let length = usize::try_from(u32::from_be_bytes(
                bytes[offset..offset + 4].try_into().expect("frame length"),
            ))
            .expect("supported length");
            let payload_start = offset + FRAME_HEADER_BYTES;
            let payload_end = payload_start + length;
            records.push(
                serde_json::from_slice::<JournalRecord>(&bytes[payload_start..payload_end])
                    .expect("journal record"),
            );
            offset = payload_end;
        }

        assert_eq!(records.len(), 3, "initial snapshot plus one commit batch");
        assert!(matches!(records[1], JournalRecord::Event { .. }));
        assert!(matches!(
            records[2],
            JournalRecord::Snapshot { cursor: 1, .. }
        ));
        let replay = replay_from_cursor(directory.path(), &current.id, 0).expect("replay");
        assert_eq!(replay, current.events);
        verify_chain(&replay).expect("hash chain");
    }

    #[test]
    fn torn_snapshot_in_commit_batch_discards_uncommitted_event() {
        let directory = tempfile::tempdir().expect("repository");
        let mut current = session(directory.path());
        let objective = current.objective.clone();
        append_payload_committed(
            &mut current,
            Actor::Coordinator,
            EventPayload::SessionCreated { objective },
        )
        .expect("committed append");
        let path = journal_path(directory.path(), &current.id).expect("journal path");
        let bytes = fs::read(&path).expect("journal bytes");

        let mut offset = JOURNAL_MAGIC.len();
        let mut frame_starts = Vec::new();
        while offset < bytes.len() {
            frame_starts.push(offset);
            let length = usize::try_from(u32::from_be_bytes(
                bytes[offset..offset + 4].try_into().expect("frame length"),
            ))
            .expect("supported length");
            offset += FRAME_HEADER_BYTES + length;
        }
        assert_eq!(frame_starts.len(), 3);
        let final_snapshot_start = frame_starts[2];
        fs::write(
            &path,
            &bytes[..final_snapshot_start + FRAME_HEADER_BYTES + 1],
        )
        .expect("tear final snapshot frame");

        let empty = session(directory.path());
        let mut compatibility = empty.clone();
        compatibility.id = current.id.clone();
        let outcome = load_or_migrate(directory.path(), &current.id, Some(compatibility))
            .expect("recover prior commit");
        assert!(outcome.session.events.is_empty());
        assert!(
            replay_from_cursor(directory.path(), &current.id, 0)
                .expect("replay")
                .is_empty()
        );
    }

    #[test]
    fn journal_locks_are_shared_per_session_but_isolated_across_sessions() {
        let first_repo = tempfile::tempdir().expect("first repository");
        let second_repo = tempfile::tempdir().expect("second repository");
        let first_id = SessionId::new();
        let second_id = SessionId::new();

        let first = session_lock(first_repo.path(), &first_id);
        let same = session_lock(first_repo.path(), &first_id);
        let other_session = session_lock(first_repo.path(), &second_id);
        let other_repo = session_lock(second_repo.path(), &first_id);

        assert!(Arc::ptr_eq(&first, &same));
        assert!(!Arc::ptr_eq(&first, &other_session));
        assert!(!Arc::ptr_eq(&first, &other_repo));
    }

    #[test]
    fn checksum_corruption_fails_closed() {
        let directory = tempfile::tempdir().expect("repository");
        let committed = session(directory.path());
        initialize_journal(&committed).expect("journal");
        let path = journal_path(directory.path(), &committed.id).expect("journal path");
        let mut bytes = fs::read(&path).expect("journal");
        let final_byte = bytes.last_mut().expect("payload byte");
        *final_byte ^= 1;
        fs::write(&path, bytes).expect("tamper");

        assert!(load_or_migrate(directory.path(), &committed.id, Some(committed.clone())).is_err());
    }

    #[test]
    fn verbatim_windows_repository_prefix_matches_plain_path() {
        let directory = tempfile::tempdir().expect("repository");
        let mut snapshot = session(directory.path());
        snapshot.repo = PathBuf::from(r"\\?\C:\Users\thoma");
        validate_snapshot_identity(Path::new(r"C:\Users\thoma"), &snapshot.id, &snapshot)
            .expect("verbatim and plain repository paths are equivalent");
    }

    #[test]
    fn committed_append_merges_events_from_a_stale_session_writer() {
        let directory = tempfile::tempdir().expect("repository");
        let mut primary = session(directory.path());
        let objective = primary.objective.clone();
        append_payload_committed(
            &mut primary,
            Actor::Coordinator,
            EventPayload::SessionCreated { objective },
        )
        .expect("initial committed event");
        let mut stale = primary.clone();

        append_payload_committed(
            &mut primary,
            Actor::Coordinator,
            EventPayload::GoalUpdated {
                objective: "updated objective".to_owned(),
            },
        )
        .expect("primary committed event");
        append_payload_committed(&mut stale, Actor::Coordinator, EventPayload::SessionResumed)
            .expect("stale writer merges committed prefix");

        assert_eq!(stale.events.len(), 3);
        assert_eq!(stale.events[2].sequence, 3);
        assert_eq!(
            stale.events[2].previous_hash.as_deref(),
            Some(primary.events[1].checksum.as_str())
        );
        let replay = replay_from_cursor(directory.path(), &stale.id, 0).expect("replay");
        assert_eq!(replay, stale.events);
    }

    #[test]
    fn action_cas_accepts_one_writer_and_audits_the_stale_writer() {
        let directory = tempfile::tempdir().expect("repository");
        let mut first = session(directory.path());
        let objective = first.objective.clone();
        append_payload_committed(
            &mut first,
            Actor::Coordinator,
            EventPayload::SessionCreated { objective },
        )
        .expect("session created");
        let mut second = first.clone();
        let first_action = action(
            &first,
            "first",
            1,
            SessionActionKind::FollowUp,
            json!({"text":"first"}),
        );
        let second_action = action(
            &second,
            "second",
            1,
            SessionActionKind::FollowUp,
            json!({"text":"second"}),
        );

        let accepted = append_payload_committed(
            &mut first,
            Actor::User,
            EventPayload::SessionActionAccepted {
                action: first_action,
            },
        )
        .expect("first admission");
        assert!(matches!(
            accepted.payload,
            EventPayload::SessionActionAccepted { .. }
        ));
        let rejected = append_payload_committed(
            &mut second,
            Actor::User,
            EventPayload::SessionActionAccepted {
                action: second_action,
            },
        )
        .expect("stale admission is audited");
        assert!(matches!(
            rejected.payload,
            EventPayload::SessionActionRejected {
                authoritative_revision: 2,
                ref reason,
                ..
            } if reason == "stale_revision"
        ));
        assert_eq!(second.events.len(), 3);
    }

    #[test]
    fn identical_action_admission_is_idempotent_under_the_journal_lock() {
        let directory = tempfile::tempdir().expect("repository");
        let mut first = session(directory.path());
        let objective = first.objective.clone();
        append_payload_committed(
            &mut first,
            Actor::Coordinator,
            EventPayload::SessionCreated { objective },
        )
        .expect("session created");
        let mut second = first.clone();
        let action = action(
            &first,
            "same",
            1,
            SessionActionKind::FollowUp,
            json!({"text":"same"}),
        );
        let accepted = append_payload_committed(
            &mut first,
            Actor::User,
            EventPayload::SessionActionAccepted {
                action: action.clone(),
            },
        )
        .expect("first admission");
        let replayed = append_payload_committed(
            &mut second,
            Actor::User,
            EventPayload::SessionActionAccepted { action },
        )
        .expect("duplicate admission");
        assert_eq!(accepted, replayed);
        assert_eq!(second.events.len(), 2);
    }

    #[test]
    fn replacement_requires_a_current_queued_followup() {
        let directory = tempfile::tempdir().expect("repository");
        let mut current = session(directory.path());
        let objective = current.objective.clone();
        append_payload_committed(
            &mut current,
            Actor::Coordinator,
            EventPayload::SessionCreated { objective },
        )
        .expect("session created");
        let original = action(
            &current,
            "original",
            1,
            SessionActionKind::FollowUp,
            json!({"text":"original"}),
        );
        append_payload_committed(
            &mut current,
            Actor::User,
            EventPayload::SessionActionAccepted {
                action: original.clone(),
            },
        )
        .expect("original admission");
        let replacement = action(
            &current,
            "replacement",
            2,
            SessionActionKind::ReplaceFollowUp,
            json!({
                "text":"replacement",
                "replaces_action_id": original.action_id,
            }),
        );
        let accepted = append_payload_committed(
            &mut current,
            Actor::User,
            EventPayload::SessionActionAccepted {
                action: replacement,
            },
        )
        .expect("replacement admission");
        assert!(matches!(
            accepted.payload,
            EventPayload::SessionActionAccepted { .. }
        ));
    }

    #[test]
    fn replay_cursor_returns_only_committed_events() {
        let directory = tempfile::tempdir().expect("repository");
        let mut committed = session(directory.path());
        for payload in [
            EventPayload::SessionCreated {
                objective: committed.objective.clone(),
            },
            EventPayload::SessionResumed,
        ] {
            let event = next_event(&committed, payload);
            append_and_materialize(&mut committed, event);
        }
        commit_snapshot(&committed).expect("commit");

        let replay = replay_from_cursor(directory.path(), &committed.id, 1).expect("replay");
        assert_eq!(replay, vec![committed.events[1].clone()]);
        assert!(replay_from_cursor(directory.path(), &committed.id, 3).is_err());
    }
}
