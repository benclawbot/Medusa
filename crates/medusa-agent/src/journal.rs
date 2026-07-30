use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
};

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult, SessionId};
use medusa_protocol::EventEnvelope;

use crate::session::{AgentSession, fallback_storage_root};

const JOURNAL_MAGIC: &[u8; 8] = b"MDJNL001";
const RECORD_HEADER_BYTES: usize = std::mem::size_of::<u32>();
const MAX_RECORD_BYTES: usize = 16 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AppendDisposition {
    Appended,
    Replayed,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct ReconcileOutcome {
    pub snapshot_changed: bool,
    pub recovered_torn_tail: bool,
}

#[derive(Debug)]
struct JournalContents {
    events: Vec<EventEnvelope>,
    recovered_torn_tail: bool,
}

pub(crate) fn append_record(
    session: &AgentSession,
    event: &EventEnvelope,
) -> MedusaResult<AppendDisposition> {
    event.validate()?;
    if event.session_id != session.id {
        return Err(persistence_error("journal event belongs to another session"));
    }
    if let Some(existing) = session
        .events
        .iter()
        .find(|existing| existing.event_id == event.event_id)
    {
        return if existing == event {
            Ok(AppendDisposition::Replayed)
        } else {
            Err(persistence_error(format!(
                "event id {} was reused with conflicting content",
                event.event_id
            )))
        };
    }

    validate_snapshot_binding(session)?;
    let expected_sequence = u64::try_from(session.events.len())
        .unwrap_or(u64::MAX)
        .saturating_add(1);
    if event.sequence != expected_sequence {
        return Err(persistence_error(format!(
            "journal event sequence {} does not follow snapshot cursor {}",
            event.sequence, session.applied_journal_cursor
        )));
    }
    let expected_previous = session.events.last().map(|previous| previous.checksum.as_str());
    if event.previous_hash.as_deref() != expected_previous {
        return Err(persistence_error(
            "journal event previous hash does not match the materialized snapshot",
        ));
    }

    let path = ensure_journal_for_snapshot(session)?;
    append_events(&path, std::slice::from_ref(event))?;
    Ok(AppendDisposition::Appended)
}

pub(crate) fn reconcile(session: &mut AgentSession) -> MedusaResult<ReconcileOutcome> {
    validate_event_sequence(&session.id, &session.events)?;
    validate_legacy_snapshot_binding(session)?;

    let Some(path) = existing_journal_path(&session.repo, &session.id) else {
        if session.events.is_empty() {
            return Ok(set_snapshot_binding(session, false, false));
        }
        let path = create_journal_for_snapshot(session)?;
        debug_assert!(path.is_file());
        return Ok(set_snapshot_binding(session, true, false));
    };

    let mut journal = read_journal(&path, &session.id, true)?;
    let shared = session.events.len().min(journal.events.len());
    for index in 0..shared {
        if session.events[index] != journal.events[index] {
            return Err(persistence_error(format!(
                "materialized session diverges from journal at sequence {}",
                index.saturating_add(1)
            )));
        }
    }

    let mut snapshot_changed = journal.recovered_torn_tail;
    if session.events.len() > journal.events.len() {
        let missing = &session.events[journal.events.len()..];
        append_events(&path, missing)?;
        journal.events.extend_from_slice(missing);
    } else if journal.events.len() > session.events.len() {
        let snapshot_length = session.events.len();
        session
            .events
            .extend_from_slice(&journal.events[snapshot_length..]);
        snapshot_changed = true;
    }

    let binding = set_snapshot_binding(
        session,
        snapshot_changed,
        journal.recovered_torn_tail,
    );
    Ok(binding)
}

pub(crate) fn validate_snapshot_binding(session: &AgentSession) -> MedusaResult<()> {
    validate_event_sequence(&session.id, &session.events)?;
    let expected_cursor = u64::try_from(session.events.len()).unwrap_or(u64::MAX);
    if session.applied_journal_cursor != expected_cursor {
        return Err(persistence_error(format!(
            "materialized session cursor {} does not match {} events",
            session.applied_journal_cursor,
            session.events.len()
        )));
    }
    let expected_checksum = session.events.last().map(|event| event.checksum.as_str());
    if session.applied_journal_checksum.as_deref() != expected_checksum {
        return Err(persistence_error(
            "materialized session journal checksum does not match its final event",
        ));
    }
    Ok(())
}

fn validate_legacy_snapshot_binding(session: &AgentSession) -> MedusaResult<()> {
    if session.applied_journal_cursor == 0 && session.applied_journal_checksum.is_none() {
        return Ok(());
    }
    validate_snapshot_binding(session)
}

fn set_snapshot_binding(
    session: &mut AgentSession,
    mut snapshot_changed: bool,
    recovered_torn_tail: bool,
) -> ReconcileOutcome {
    let cursor = u64::try_from(session.events.len()).unwrap_or(u64::MAX);
    let checksum = session.events.last().map(|event| event.checksum.clone());
    if session.applied_journal_cursor != cursor || session.applied_journal_checksum != checksum {
        session.applied_journal_cursor = cursor;
        session.applied_journal_checksum = checksum;
        snapshot_changed = true;
    }
    ReconcileOutcome {
        snapshot_changed,
        recovered_torn_tail,
    }
}

fn ensure_journal_for_snapshot(session: &AgentSession) -> MedusaResult<PathBuf> {
    if let Some(path) = existing_journal_path(&session.repo, &session.id) {
        return Ok(path);
    }
    create_journal_for_snapshot(session)
}

fn create_journal_for_snapshot(session: &AgentSession) -> MedusaResult<PathBuf> {
    let primary = primary_journal_path(&session.repo, &session.id);
    if write_journal(&primary, &session.events).is_ok() {
        return Ok(primary);
    }
    let fallback = fallback_journal_path(&session.repo, &session.id);
    write_journal(&fallback, &session.events)?;
    Ok(fallback)
}

fn existing_journal_path(repo: &Path, session_id: &SessionId) -> Option<PathBuf> {
    let primary = primary_journal_path(repo, session_id);
    if primary.is_file() {
        return Some(primary);
    }
    let fallback = fallback_journal_path(repo, session_id);
    fallback.is_file().then_some(fallback)
}

fn primary_journal_path(repo: &Path, session_id: &SessionId) -> PathBuf {
    repo.join(".medusa/journals")
        .join(format!("{session_id}.events"))
}

fn fallback_journal_path(repo: &Path, session_id: &SessionId) -> PathBuf {
    fallback_storage_root(repo, "journals").join(format!("{session_id}.events"))
}

fn create_parent(path: &Path) -> MedusaResult<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    Ok(())
}

fn write_journal(path: &Path, events: &[EventEnvelope]) -> MedusaResult<()> {
    validate_event_sequence_for_events(events)?;
    create_parent(path)?;
    let temporary = path.with_extension("events.tmp");
    let mut file = File::create(&temporary)?;
    file.write_all(JOURNAL_MAGIC)?;
    write_records(&mut file, events)?;
    file.sync_all()?;
    fs::rename(&temporary, path)?;
    Ok(())
}

fn append_events(path: &Path, events: &[EventEnvelope]) -> MedusaResult<()> {
    if events.is_empty() {
        return Ok(());
    }
    create_parent(path)?;
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .read(true)
        .open(path)?;
    if file.metadata()?.len() == 0 {
        file.write_all(JOURNAL_MAGIC)?;
    }
    write_records(&mut file, events)?;
    file.sync_data()?;
    Ok(())
}

fn write_records(file: &mut File, events: &[EventEnvelope]) -> MedusaResult<()> {
    for event in events {
        event.validate()?;
        let bytes = serde_json::to_vec(event)?;
        let length = u32::try_from(bytes.len())
            .map_err(|_| persistence_error("journal event exceeds the framing limit"))?;
        if bytes.len() > MAX_RECORD_BYTES {
            return Err(persistence_error("journal event exceeds the framing limit"));
        }
        file.write_all(&length.to_be_bytes())?;
        file.write_all(&bytes)?;
    }
    Ok(())
}

fn read_journal(
    path: &Path,
    session_id: &SessionId,
    recover_torn_tail: bool,
) -> MedusaResult<JournalContents> {
    let mut bytes = Vec::new();
    File::open(path)?.read_to_end(&mut bytes)?;
    if bytes.len() < JOURNAL_MAGIC.len() || &bytes[..JOURNAL_MAGIC.len()] != JOURNAL_MAGIC {
        return Err(persistence_error("journal header is missing or unsupported"));
    }

    let mut offset = JOURNAL_MAGIC.len();
    let mut valid_length = offset;
    let mut events = Vec::new();
    let mut recovered_torn_tail = false;
    while offset < bytes.len() {
        let remaining = bytes.len() - offset;
        if remaining < RECORD_HEADER_BYTES {
            recovered_torn_tail = true;
            break;
        }
        let length = usize::try_from(u32::from_be_bytes(
            bytes[offset..offset + RECORD_HEADER_BYTES]
                .try_into()
                .map_err(|_| persistence_error("journal record header is invalid"))?,
        ))
        .map_err(|_| persistence_error("journal record length is unsupported"))?;
        if length == 0 || length > MAX_RECORD_BYTES {
            return Err(persistence_error("journal record length is invalid"));
        }
        let payload_start = offset + RECORD_HEADER_BYTES;
        let Some(payload_end) = payload_start.checked_add(length) else {
            return Err(persistence_error("journal record length overflowed"));
        };
        if payload_end > bytes.len() {
            recovered_torn_tail = true;
            break;
        }
        let event: EventEnvelope = serde_json::from_slice(&bytes[payload_start..payload_end])?;
        events.push(event);
        offset = payload_end;
        valid_length = offset;
    }

    validate_event_sequence(session_id, &events)?;
    if recovered_torn_tail {
        if !recover_torn_tail {
            return Err(persistence_error("journal has a torn final record"));
        }
        let file = OpenOptions::new().write(true).open(path)?;
        let valid_length = u64::try_from(valid_length)
            .map_err(|_| persistence_error("journal length is unsupported"))?;
        file.set_len(valid_length)?;
        file.sync_data()?;
    }
    Ok(JournalContents {
        events,
        recovered_torn_tail,
    })
}

fn validate_event_sequence(session_id: &SessionId, events: &[EventEnvelope]) -> MedusaResult<()> {
    let mut event_ids = BTreeMap::new();
    let mut previous_hash: Option<&str> = None;
    for (index, event) in events.iter().enumerate() {
        event.validate()?;
        if &event.session_id != session_id {
            return Err(persistence_error("journal contains an event for another session"));
        }
        let expected_sequence = u64::try_from(index)
            .unwrap_or(u64::MAX)
            .saturating_add(1);
        if event.sequence != expected_sequence {
            return Err(persistence_error(format!(
                "journal sequence {} is not the expected sequence {expected_sequence}",
                event.sequence
            )));
        }
        if event.previous_hash.as_deref() != previous_hash {
            return Err(persistence_error("journal previous hash chain is invalid"));
        }
        if let Some(previous) = event_ids.insert(event.event_id.as_str(), event) {
            if previous != event {
                return Err(persistence_error(format!(
                    "event id {} was reused with conflicting content",
                    event.event_id
                )));
            }
            return Err(persistence_error(format!(
                "event id {} appears more than once",
                event.event_id
            )));
        }
        previous_hash = Some(&event.checksum);
    }
    Ok(())
}

fn validate_event_sequence_for_events(events: &[EventEnvelope]) -> MedusaResult<()> {
    if let Some(first) = events.first() {
        validate_event_sequence(&first.session_id, events)
    } else {
        Ok(())
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
    use std::io::Write;

    use medusa_core::CorrelationId;
    use medusa_protocol::{Actor, EventPayload};
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
            messages: Vec::new(),
            events: Vec::new(),
            applied_journal_cursor: 0,
            applied_journal_checksum: None,
            evidence: Vec::new(),
            tool_artifacts: Vec::new(),
            world_model: None,
            approval_grants: Vec::new(),
            approval_receipts: Vec::new(),
            rollback_receipts: Vec::new(),
        }
    }

    fn event(session: &AgentSession, payload: EventPayload) -> EventEnvelope {
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

    fn apply(session: &mut AgentSession, event: EventEnvelope) {
        assert_eq!(
            append_record(session, &event).expect("append"),
            AppendDisposition::Appended
        );
        session.events.push(event);
        session.applied_journal_cursor = session.events.len() as u64;
        session.applied_journal_checksum =
            session.events.last().map(|event| event.checksum.clone());
    }

    #[test]
    fn ordered_append_and_cursor_replay() {
        let directory = tempfile::tempdir().expect("repository");
        let mut original = session(directory.path());
        let created = event(
            &original,
            EventPayload::SessionCreated {
                objective: original.objective.clone(),
            },
        );
        apply(&mut original, created);
        let resumed = event(&original, EventPayload::SessionResumed);
        apply(&mut original, resumed);

        let mut stale = session(directory.path());
        stale.id = original.id.clone();
        stale.objective = original.objective.clone();
        let outcome = reconcile(&mut stale).expect("reconcile");

        assert!(outcome.snapshot_changed);
        assert_eq!(stale.events, original.events);
        assert_eq!(stale.applied_journal_cursor, 2);
        assert_eq!(
            stale.applied_journal_checksum,
            original.events.last().map(|event| event.checksum.clone())
        );
    }

    #[test]
    fn identical_event_replay_is_idempotent_and_conflicts_fail_closed() {
        let directory = tempfile::tempdir().expect("repository");
        let mut session = session(directory.path());
        let created = event(
            &session,
            EventPayload::SessionCreated {
                objective: session.objective.clone(),
            },
        );
        apply(&mut session, created.clone());

        assert_eq!(
            append_record(&session, &created).expect("replay"),
            AppendDisposition::Replayed
        );

        let mut conflicting = created;
        conflicting.payload = EventPayload::SessionFailed {
            error: persistence_error("conflicting payload"),
        };
        conflicting.checksum = conflicting.compute_checksum().expect("checksum");
        assert!(append_record(&session, &conflicting).is_err());
    }

    #[test]
    fn torn_final_record_is_removed_without_losing_valid_events() {
        let directory = tempfile::tempdir().expect("repository");
        let mut session = session(directory.path());
        let created = event(
            &session,
            EventPayload::SessionCreated {
                objective: session.objective.clone(),
            },
        );
        apply(&mut session, created);
        let path = primary_journal_path(directory.path(), &session.id);
        let valid_length = fs::metadata(&path).expect("metadata").len();
        OpenOptions::new()
            .append(true)
            .open(&path)
            .expect("journal")
            .write_all(&12_u32.to_be_bytes())
            .expect("partial header");

        let mut snapshot = session.clone();
        let outcome = reconcile(&mut snapshot).expect("recover");

        assert!(outcome.recovered_torn_tail);
        assert_eq!(fs::metadata(path).expect("metadata").len(), valid_length);
        assert_eq!(snapshot.events, session.events);
    }

    #[test]
    fn modified_complete_record_is_rejected_instead_of_truncated() {
        let directory = tempfile::tempdir().expect("repository");
        let mut session = session(directory.path());
        let created = event(
            &session,
            EventPayload::SessionCreated {
                objective: session.objective.clone(),
            },
        );
        apply(&mut session, created);
        let path = primary_journal_path(directory.path(), &session.id);
        let mut bytes = fs::read(&path).expect("journal");
        let final_byte = bytes.last_mut().expect("payload byte");
        *final_byte ^= 1;
        fs::write(&path, bytes).expect("tamper");

        assert!(reconcile(&mut session).is_err());
    }
}
