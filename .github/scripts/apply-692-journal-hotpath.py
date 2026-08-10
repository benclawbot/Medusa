from pathlib import Path

path = Path("crates/medusa-agent/src/journal.rs")
text = path.read_text()

def once(old, new):
    global text
    count = text.count(old)
    if count != 1:
        raise SystemExit(f"expected one match, found {count}: {old[:120]!r}")
    text = text.replace(old, new, 1)

once(
"""    collections::BTreeSet,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{Mutex, MutexGuard, OnceLock},
""",
"""    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard, OnceLock, Weak},
""",
)
once(
"static JOURNAL_WRITE_LOCK: OnceLock<Mutex<()>> = OnceLock::new();",
"static JOURNAL_LOCKS: OnceLock<Mutex<BTreeMap<String, Weak<Mutex<()>>>>> = OnceLock::new();",
)
once(
"""    let _guard = lock_journal();
    verify_chain(&session.events)?;
    ensure_initialized(session)?;
""",
"""    let lock = session_lock(&session.repo, &session.id);
    let _guard = lock_mutex(&lock);
    verify_chain(&session.events)?;
    ensure_initialized(session)?;
""",
)
once(
"""    append_record(
        &path,
        &JournalRecord::Event {
            event: Box::new(event.clone()),
        },
    )?;
    session.events.push(event.clone());
    append_record(&path, &snapshot_record(session))?;
    Ok(event)
""",
"""    let mut committed = session.clone();
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
""",
)
for old, new in [
(
"""pub(crate) fn append_event(
    session: &AgentSession,
    event: &EventEnvelope,
) -> MedusaResult<AppendDisposition> {
    let _guard = lock_journal();
""",
"""pub(crate) fn append_event(
    session: &AgentSession,
    event: &EventEnvelope,
) -> MedusaResult<AppendDisposition> {
    let lock = session_lock(&session.repo, &session.id);
    let _guard = lock_mutex(&lock);
"""
),
(
"""pub(crate) fn commit_snapshot(session: &AgentSession) -> MedusaResult<AgentSession> {
    let _guard = lock_journal();
""",
"""pub(crate) fn commit_snapshot(session: &AgentSession) -> MedusaResult<AgentSession> {
    let lock = session_lock(&session.repo, &session.id);
    let _guard = lock_mutex(&lock);
"""
),
(
"""where
    F: FnOnce(&AgentSession) -> MedusaResult<()>,
{
    let _guard = lock_journal();
""",
"""where
    F: FnOnce(&AgentSession) -> MedusaResult<()>,
{
    let lock = session_lock(&session.repo, &session.id);
    let _guard = lock_mutex(&lock);
"""
),
(
"""pub(crate) fn load_or_migrate(
    repo: &Path,
    session_id: &SessionId,
    snapshot: Option<AgentSession>,
) -> MedusaResult<LoadOutcome> {
    let _guard = lock_journal();
""",
"""pub(crate) fn load_or_migrate(
    repo: &Path,
    session_id: &SessionId,
    snapshot: Option<AgentSession>,
) -> MedusaResult<LoadOutcome> {
    let lock = session_lock(repo, session_id);
    let _guard = lock_mutex(&lock);
"""
),
(
"""pub(crate) fn replay_from_cursor(
    repo: &Path,
    session_id: &SessionId,
    cursor: u64,
) -> MedusaResult<Vec<EventEnvelope>> {
    let _guard = lock_journal();
""",
"""pub(crate) fn replay_from_cursor(
    repo: &Path,
    session_id: &SessionId,
    cursor: u64,
) -> MedusaResult<Vec<EventEnvelope>> {
    let lock = session_lock(repo, session_id);
    let _guard = lock_mutex(&lock);
"""
)
]:
    once(old,new)

once(
"""fn snapshot_record(session: &AgentSession) -> JournalRecord {
    JournalRecord::Snapshot {
        cursor: u64::try_from(session.events.len()).unwrap_or(u64::MAX),
        final_event_checksum: session.events.last().map(|event| event.checksum.clone()),
        session: Box::new(session.clone()),
    }
}

fn append_record(path: &Path, record: &JournalRecord) -> MedusaResult<()> {
    create_parent(path)?;
    let mut file = OpenOptions::new().append(true).open(path)?;
    write_record(&mut file, record)?;
    file.sync_data()?;
    Ok(())
}
""",
"""fn snapshot_record(session: &AgentSession) -> JournalRecord {
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
    Ok(())
}
"""
)
once(
"""fn lock_journal() -> MutexGuard<'static, ()> {
    match JOURNAL_WRITE_LOCK.get_or_init(|| Mutex::new(())).lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}
""",
"""fn session_lock(repo: &Path, session_id: &SessionId) -> Arc<Mutex<()>> {
    let key = format!("{}\\0{session_id}", repo.display());
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
"""
)
path.write_text(text)
