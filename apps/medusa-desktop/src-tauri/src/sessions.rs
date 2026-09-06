use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
    time::UNIX_EPOCH,
};

use serde::Serialize;
use serde_json::Value;

const MAX_DESKTOP_SESSIONS: usize = 2_000;
const MAX_DESKTOP_SESSION_MESSAGES: usize = 2_000;
const DEFAULT_SESSION_PAGE_SIZE: usize = 50;
const DEFAULT_MESSAGE_PAGE_SIZE: usize = 100;
const MAX_PAGE_SIZE: usize = 200;
const SESSION_CURSOR_SEPARATOR: char = '\u{1f}';

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopSessionSummary {
    pub id: String,
    pub objective: String,
    pub created_at: String,
    pub updated_at: String,
    pub completed: bool,
    pub waiting_for_user: bool,
    pub turn: u32,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopSessionMessage {
    pub role: String,
    pub text: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopSessionDetail {
    pub summary: DesktopSessionSummary,
    pub messages: Vec<DesktopSessionMessage>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopSessionPage {
    pub sessions: Vec<DesktopSessionSummary>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopSessionMessagePage {
    pub summary: DesktopSessionSummary,
    pub messages: Vec<DesktopSessionMessage>,
    pub next_cursor: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FileFingerprint {
    len: u64,
    modified_nanos: u128,
}

#[derive(Clone, Debug)]
struct CachedSummary {
    fingerprint: FileFingerprint,
    summary: Option<DesktopSessionSummary>,
}

#[derive(Default)]
struct RootIndex {
    entries: BTreeMap<PathBuf, CachedSummary>,
}

#[derive(Clone, Debug)]
struct MessageIndex {
    fingerprint: FileFingerprint,
    summary: DesktopSessionSummary,
    ranges: Vec<(u64, u64)>,
}

static SUMMARY_INDEXES: OnceLock<Mutex<BTreeMap<PathBuf, RootIndex>>> = OnceLock::new();
static MESSAGE_INDEXES: OnceLock<Mutex<BTreeMap<PathBuf, MessageIndex>>> = OnceLock::new();

fn summary_indexes() -> &'static Mutex<BTreeMap<PathBuf, RootIndex>> {
    SUMMARY_INDEXES.get_or_init(|| Mutex::new(BTreeMap::new()))
}

fn message_indexes() -> &'static Mutex<BTreeMap<PathBuf, MessageIndex>> {
    MESSAGE_INDEXES.get_or_init(|| Mutex::new(BTreeMap::new()))
}

/// Compatibility wrapper. New desktop callers should use `runtime_list_sessions_page` so the
/// authority controls page size and can reuse indexed metadata between requests.
#[tauri::command]
pub async fn runtime_list_sessions(repo: String) -> Result<Vec<DesktopSessionSummary>, String> {
    run_blocking(move || {
        Ok(list_sessions_page_sync(&repo, None, MAX_DESKTOP_SESSIONS)?.sessions)
    })
    .await
}

#[tauri::command]
pub async fn runtime_list_sessions_page(
    repo: String,
    cursor: Option<String>,
    limit: Option<usize>,
) -> Result<DesktopSessionPage, String> {
    run_blocking(move || list_sessions_page_sync(&repo, cursor.as_deref(), page_limit(limit, DEFAULT_SESSION_PAGE_SIZE)))
        .await
}

/// Compatibility wrapper returning the newest bounded message window.
#[tauri::command]
pub async fn runtime_read_session(
    repo: String,
    session_id: String,
) -> Result<DesktopSessionDetail, String> {
    run_blocking(move || {
        let page = read_session_page_sync(
            &repo,
            &session_id,
            None,
            MAX_DESKTOP_SESSION_MESSAGES,
        )?;
        Ok(DesktopSessionDetail {
            summary: page.summary,
            messages: page.messages,
        })
    })
    .await
}

#[tauri::command]
pub async fn runtime_read_session_page(
    repo: String,
    session_id: String,
    cursor: Option<String>,
    limit: Option<usize>,
) -> Result<DesktopSessionMessagePage, String> {
    run_blocking(move || {
        read_session_page_sync(
            &repo,
            &session_id,
            cursor.as_deref(),
            page_limit(limit, DEFAULT_MESSAGE_PAGE_SIZE),
        )
    })
    .await
}

async fn run_blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    tauri::async_runtime::spawn_blocking(work)
        .await
        .map_err(|error| format!("desktop session worker failed: {error}"))?
}

fn page_limit(limit: Option<usize>, default: usize) -> usize {
    limit.unwrap_or(default).clamp(1, MAX_PAGE_SIZE)
}

fn list_sessions_page_sync(
    repo: &str,
    cursor: Option<&str>,
    limit: usize,
) -> Result<DesktopSessionPage, String> {
    let repo = canonical_repo(repo)?;
    let mut sessions = BTreeMap::new();
    collect_sessions_indexed(&repo.join(".medusa/sessions"), &mut sessions)?;
    collect_sessions_indexed(&fallback_session_root(&repo), &mut sessions)?;
    let mut sessions = sessions.into_values().collect::<Vec<_>>();
    sessions.sort_by(|left, right| {
        right
            .updated_at
            .cmp(&left.updated_at)
            .then_with(|| left.id.cmp(&right.id))
    });

    if let Some(cursor) = cursor {
        let (updated_at, id) = decode_session_cursor(cursor)?;
        sessions.retain(|session| {
            session.updated_at < updated_at
                || (session.updated_at == updated_at && session.id > id)
        });
    }

    let has_more = sessions.len() > limit;
    sessions.truncate(limit);
    let next_cursor = has_more
        .then(|| sessions.last().map(encode_session_cursor))
        .flatten();
    Ok(DesktopSessionPage {
        sessions,
        next_cursor,
    })
}

fn read_session_page_sync(
    repo: &str,
    session_id: &str,
    cursor: Option<&str>,
    limit: usize,
) -> Result<DesktopSessionMessagePage, String> {
    let repo = canonical_repo(repo)?;
    let path = find_session_path(&repo, session_id)?;
    let index = message_index(&path, session_id)?;
    let end = match cursor {
        Some(cursor) => cursor
            .parse::<usize>()
            .map_err(|_| "invalid session message cursor".to_owned())?
            .min(index.ranges.len()),
        None => index.ranges.len(),
    };
    let start = end.saturating_sub(limit);
    let mut file = File::open(&path)
        .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    let mut messages = Vec::with_capacity(end.saturating_sub(start));
    for &(range_start, range_end) in &index.ranges[start..end] {
        let len = usize::try_from(range_end.saturating_sub(range_start))
            .map_err(|_| format!("session message in {} is too large", path.display()))?;
        let mut buffer = vec![0_u8; len];
        file.seek(SeekFrom::Start(range_start))
            .and_then(|_| file.read_exact(&mut buffer))
            .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
        let value: Value = serde_json::from_slice(&buffer)
            .map_err(|error| format!("cannot parse {}: {error}", path.display()))?;
        if let Some(message) = message_from_value(&value) {
            messages.push(message);
        }
    }
    Ok(DesktopSessionMessagePage {
        summary: index.summary,
        messages,
        next_cursor: (start > 0).then(|| start.to_string()),
    })
}

fn canonical_repo(repo: &str) -> Result<PathBuf, String> {
    let repo = fs::canonicalize(Path::new(repo))
        .map_err(|error| format!("cannot open {repo}: {error}"))?;
    if !repo.is_dir() {
        return Err(format!("{} is not a directory", repo.display()));
    }
    Ok(repo)
}

fn find_session_path(repo: &Path, session_id: &str) -> Result<PathBuf, String> {
    validate_session_id(session_id)?;
    for root in [repo.join(".medusa/sessions"), fallback_session_root(repo)] {
        let path = root.join(format!("{session_id}.json"));
        if path.is_file() {
            return Ok(path);
        }
    }
    Err(format!(
        "session {session_id} was not found for {}",
        repo.display()
    ))
}

fn validate_session_id(session_id: &str) -> Result<(), String> {
    if session_id.is_empty()
        || !session_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
    {
        return Err("invalid session id".to_owned());
    }
    Ok(())
}

fn collect_sessions_indexed(
    root: &Path,
    sessions: &mut BTreeMap<String, DesktopSessionSummary>,
) -> Result<(), String> {
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(format!("cannot read {}: {error}", root.display())),
    };
    let mut seen = BTreeSet::new();
    let mut index_guard = summary_indexes()
        .lock()
        .map_err(|_| "desktop session index lock is poisoned".to_owned())?;
    let index = index_guard.entry(root.to_path_buf()).or_default();

    for entry in entries {
        let entry = entry.map_err(|error| format!("cannot read session entry: {error}"))?;
        let path = entry.path();
        let metadata = match entry.metadata() {
            Ok(metadata) if metadata.is_file() => metadata,
            Ok(_) => continue,
            Err(_) => continue,
        };
        if path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        seen.insert(path.clone());
        let fingerprint = fingerprint(&metadata);
        let cached = index.entries.get(&path).filter(|entry| entry.fingerprint == fingerprint);
        let summary = if let Some(cached) = cached {
            cached.summary.clone()
        } else {
            // Malformed files are isolated as an empty index entry and retried only after their
            // metadata changes. One corrupt session must not hide the rest of the history.
            let summary = read_summary(&path).ok().flatten();
            index.entries.insert(
                path.clone(),
                CachedSummary {
                    fingerprint,
                    summary: summary.clone(),
                },
            );
            summary
        };
        if let Some(summary) = summary {
            sessions.entry(summary.id.clone()).or_insert(summary);
        }
    }
    index.entries.retain(|path, _| seen.contains(path));
    Ok(())
}

fn read_summary(path: &Path) -> Result<Option<DesktopSessionSummary>, String> {
    let file = File::open(path)
        .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    let value: Value = serde_json::from_reader(file)
        .map_err(|error| format!("cannot parse {}: {error}", path.display()))?;
    Ok(summary_from_value(&value))
}

fn message_index(path: &Path, session_id: &str) -> Result<MessageIndex, String> {
    let metadata = fs::metadata(path)
        .map_err(|error| format!("cannot inspect {}: {error}", path.display()))?;
    let fingerprint = fingerprint(&metadata);
    if let Some(cached) = message_indexes()
        .lock()
        .map_err(|_| "desktop message index lock is poisoned".to_owned())?
        .get(path)
        .filter(|cached| cached.fingerprint == fingerprint)
        .cloned()
    {
        return Ok(cached);
    }

    let bytes = fs::read(path)
        .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    let value: Value = serde_json::from_slice(&bytes)
        .map_err(|error| format!("cannot parse {}: {error}", path.display()))?;
    if value.get("id").and_then(Value::as_str) != Some(session_id) {
        return Err(format!("session {session_id} has mismatched durable metadata"));
    }
    let summary = summary_from_value(&value)
        .ok_or_else(|| format!("session {session_id} is missing required metadata"))?;
    let ranges = locate_message_ranges(&bytes)
        .map_err(|error| format!("cannot index {}: {error}", path.display()))?;
    let index = MessageIndex {
        fingerprint,
        summary,
        ranges,
    };
    message_indexes()
        .lock()
        .map_err(|_| "desktop message index lock is poisoned".to_owned())?
        .insert(path.to_path_buf(), index.clone());
    Ok(index)
}

fn fingerprint(metadata: &fs::Metadata) -> FileFingerprint {
    let modified_nanos = metadata
        .modified()
        .ok()
        .and_then(|value| value.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |duration| duration.as_nanos());
    FileFingerprint {
        len: metadata.len(),
        modified_nanos,
    }
}

fn encode_session_cursor(summary: &DesktopSessionSummary) -> String {
    format!(
        "{}{}{}",
        summary.updated_at, SESSION_CURSOR_SEPARATOR, summary.id
    )
}

fn decode_session_cursor(cursor: &str) -> Result<(&str, &str), String> {
    cursor
        .split_once(SESSION_CURSOR_SEPARATOR)
        .filter(|(updated_at, id)| !updated_at.is_empty() && !id.is_empty())
        .ok_or_else(|| "invalid session cursor".to_owned())
}

fn summary_from_value(value: &Value) -> Option<DesktopSessionSummary> {
    Some(DesktopSessionSummary {
        id: value.get("id")?.as_str()?.to_owned(),
        objective: value.get("objective")?.as_str()?.to_owned(),
        created_at: value.get("created_at")?.as_str()?.to_owned(),
        updated_at: value.get("updated_at")?.as_str()?.to_owned(),
        completed: value.get("completed")?.as_bool()?,
        waiting_for_user: value
            .get("pending_question")
            .is_some_and(|question| !question.is_null()),
        turn: u32::try_from(value.get("turn")?.as_u64()?).ok()?,
    })
}

fn message_from_value(value: &Value) -> Option<DesktopSessionMessage> {
    let role = value.get("role")?.as_str()?.to_owned();
    let text = value
        .get("content")?
        .as_array()?
        .iter()
        .filter_map(block_text)
        .collect::<Vec<_>>()
        .join("\n");
    (!text.trim().is_empty()).then_some(DesktopSessionMessage { role, text })
}

fn block_text(value: &Value) -> Option<String> {
    match value.get("type")?.as_str()? {
        "text" => value.get("text")?.as_str().map(str::to_owned),
        "image" => Some("[Image attachment]".to_owned()),
        "tool_use" => Some(format!(
            "Tool: {}",
            value
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
        )),
        "tool_result" => value.get("content")?.as_str().map(str::to_owned),
        _ => None,
    }
}

/// Return byte ranges for top-level `messages` array elements without materializing them.
/// The scanner understands JSON string escaping and nested arrays/objects, so subsequent page
/// reads can seek directly to the requested messages rather than reparsing the whole session.
fn locate_message_ranges(bytes: &[u8]) -> Result<Vec<(u64, u64)>, String> {
    let mut cursor = skip_ws(bytes, 0);
    if bytes.get(cursor) != Some(&b'{') {
        return Err("session root is not an object".to_owned());
    }
    cursor += 1;
    loop {
        cursor = skip_ws(bytes, cursor);
        if bytes.get(cursor) == Some(&b'}') {
            return Ok(Vec::new());
        }
        if bytes.get(cursor) != Some(&b'"') {
            return Err("invalid object key".to_owned());
        }
        let key_end = scan_string_end(bytes, cursor)?;
        let key: String = serde_json::from_slice(&bytes[cursor..key_end])
            .map_err(|error| format!("invalid object key: {error}"))?;
        cursor = skip_ws(bytes, key_end);
        if bytes.get(cursor) != Some(&b':') {
            return Err("object key is missing ':'".to_owned());
        }
        cursor = skip_ws(bytes, cursor + 1);
        if key == "messages" {
            return scan_array_ranges(bytes, cursor);
        }
        cursor = scan_value_end(bytes, cursor)?;
        cursor = skip_ws(bytes, cursor);
        match bytes.get(cursor) {
            Some(b',') => cursor += 1,
            Some(b'}') => return Ok(Vec::new()),
            _ => return Err("invalid object separator".to_owned()),
        }
    }
}

fn scan_array_ranges(bytes: &[u8], mut cursor: usize) -> Result<Vec<(u64, u64)>, String> {
    if bytes.get(cursor) != Some(&b'[') {
        return Err("messages is not an array".to_owned());
    }
    cursor += 1;
    let mut ranges = Vec::new();
    loop {
        cursor = skip_ws(bytes, cursor);
        if bytes.get(cursor) == Some(&b']') {
            return Ok(ranges);
        }
        let start = cursor;
        let end = scan_value_end(bytes, start)?;
        ranges.push((
            u64::try_from(start).map_err(|_| "message offset overflow".to_owned())?,
            u64::try_from(end).map_err(|_| "message offset overflow".to_owned())?,
        ));
        cursor = skip_ws(bytes, end);
        match bytes.get(cursor) {
            Some(b',') => cursor += 1,
            Some(b']') => return Ok(ranges),
            _ => return Err("invalid messages separator".to_owned()),
        }
    }
}

fn scan_value_end(bytes: &[u8], cursor: usize) -> Result<usize, String> {
    let cursor = skip_ws(bytes, cursor);
    match bytes.get(cursor) {
        Some(b'"') => scan_string_end(bytes, cursor),
        Some(b'{') | Some(b'[') => scan_compound_end(bytes, cursor),
        Some(_) => {
            let mut end = cursor;
            while let Some(byte) = bytes.get(end) {
                if matches!(byte, b',' | b']' | b'}' | b' ' | b'\n' | b'\r' | b'\t') {
                    break;
                }
                end += 1;
            }
            (end > cursor)
                .then_some(end)
                .ok_or_else(|| "invalid JSON value".to_owned())
        }
        None => Err("unexpected end of JSON".to_owned()),
    }
}

fn scan_compound_end(bytes: &[u8], cursor: usize) -> Result<usize, String> {
    let mut stack = vec![bytes[cursor]];
    let mut index = cursor + 1;
    while let Some(byte) = bytes.get(index).copied() {
        match byte {
            b'"' => index = scan_string_end(bytes, index)?,
            b'{' | b'[' => {
                stack.push(byte);
                index += 1;
            }
            b'}' => {
                if stack.pop() != Some(b'{') {
                    return Err("mismatched JSON object".to_owned());
                }
                index += 1;
                if stack.is_empty() {
                    return Ok(index);
                }
            }
            b']' => {
                if stack.pop() != Some(b'[') {
                    return Err("mismatched JSON array".to_owned());
                }
                index += 1;
                if stack.is_empty() {
                    return Ok(index);
                }
            }
            _ => index += 1,
        }
    }
    Err("unterminated JSON value".to_owned())
}

fn scan_string_end(bytes: &[u8], cursor: usize) -> Result<usize, String> {
    let mut index = cursor + 1;
    let mut escaped = false;
    while let Some(byte) = bytes.get(index).copied() {
        if escaped {
            escaped = false;
            index += 1;
            continue;
        }
        match byte {
            b'\\' => escaped = true,
            b'"' => return Ok(index + 1),
            _ => {}
        }
        index += 1;
    }
    Err("unterminated JSON string".to_owned())
}

fn skip_ws(bytes: &[u8], mut cursor: usize) -> usize {
    while bytes
        .get(cursor)
        .is_some_and(|byte| matches!(byte, b' ' | b'\n' | b'\r' | b'\t'))
    {
        cursor += 1;
    }
    cursor
}

fn fallback_session_root(repo: &Path) -> PathBuf {
    let root = std::env::var_os("LOCALAPPDATA")
        .or_else(|| std::env::var_os("APPDATA"))
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
        .unwrap_or_else(std::env::temp_dir);
    root.join("Medusa/sessions").join(repository_key(repo))
}

fn repository_key(repo: &Path) -> String {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in repo.to_string_lossy().as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_session(root: &Path, id: &str, updated: &str, messages: usize) {
        fs::create_dir_all(root).expect("session root");
        let items = (0..messages)
            .map(|index| {
                serde_json::json!({
                    "role": if index % 2 == 0 { "user" } else { "assistant" },
                    "content": [{"type": "text", "text": format!("message {index}")}]
                })
            })
            .collect::<Vec<_>>();
        let value = serde_json::json!({
            "id": id,
            "objective": format!("objective {id}"),
            "created_at": "2026-01-01T00:00:00Z",
            "updated_at": updated,
            "completed": false,
            "pending_question": null,
            "turn": 1,
            "messages": items,
        });
        fs::write(
            root.join(format!("{id}.json")),
            serde_json::to_vec(&value).expect("serialize session"),
        )
        .expect("write session");
    }

    #[test]
    fn session_pages_are_stable_and_corrupt_items_are_isolated() {
        let repo = crate::tempdir().expect("repo");
        let root = repo.path().join(".medusa/sessions");
        write_session(&root, "a", "2026-01-01T00:00:01Z", 1);
        write_session(&root, "b", "2026-01-01T00:00:03Z", 1);
        write_session(&root, "c", "2026-01-01T00:00:02Z", 1);
        fs::write(root.join("corrupt.json"), b"{not-json").expect("corrupt fixture");

        let repo_text = repo.path().to_string_lossy();
        let first = list_sessions_page_sync(&repo_text, None, 2).expect("first page");
        assert_eq!(
            first.sessions.iter().map(|item| item.id.as_str()).collect::<Vec<_>>(),
            vec!["b", "c"]
        );
        let second = list_sessions_page_sync(&repo_text, first.next_cursor.as_deref(), 2)
            .expect("second page");
        assert_eq!(
            second.sessions.iter().map(|item| item.id.as_str()).collect::<Vec<_>>(),
            vec!["a"]
        );
        assert!(second.next_cursor.is_none());
    }

    #[test]
    fn changed_session_metadata_invalidates_only_that_index_entry() {
        let repo = crate::tempdir().expect("repo");
        let root = repo.path().join(".medusa/sessions");
        write_session(&root, "a", "2026-01-01T00:00:01Z", 1);
        write_session(&root, "b", "2026-01-01T00:00:02Z", 1);
        let repo_text = repo.path().to_string_lossy();
        let initial = list_sessions_page_sync(&repo_text, None, 10).expect("initial");
        assert_eq!(initial.sessions[0].id, "b");

        // Changing the message count also changes the file length, making this deterministic on
        // filesystems whose modification timestamp granularity is coarse.
        write_session(&root, "a", "2026-01-01T00:00:04Z", 3);
        let refreshed = list_sessions_page_sync(&repo_text, None, 10).expect("refreshed");
        assert_eq!(refreshed.sessions[0].id, "a");
    }

    #[test]
    fn message_pages_seek_to_bounded_ranges_and_keep_older_content_accessible() {
        let repo = crate::tempdir().expect("repo");
        let root = repo.path().join(".medusa/sessions");
        write_session(&root, "history", "2026-01-01T00:00:01Z", 7);
        let repo_text = repo.path().to_string_lossy();

        let newest = read_session_page_sync(&repo_text, "history", None, 3).expect("newest");
        assert_eq!(
            newest.messages.iter().map(|item| item.text.as_str()).collect::<Vec<_>>(),
            vec!["message 4", "message 5", "message 6"]
        );
        let older = read_session_page_sync(
            &repo_text,
            "history",
            newest.next_cursor.as_deref(),
            3,
        )
        .expect("older");
        assert_eq!(
            older.messages.iter().map(|item| item.text.as_str()).collect::<Vec<_>>(),
            vec!["message 1", "message 2", "message 3"]
        );
        let oldest = read_session_page_sync(
            &repo_text,
            "history",
            older.next_cursor.as_deref(),
            3,
        )
        .expect("oldest");
        assert_eq!(oldest.messages[0].text, "message 0");
        assert!(oldest.next_cursor.is_none());
    }

    #[test]
    fn message_range_scanner_handles_nested_content_and_escaped_strings() {
        let value = br#"{"id":"x","messages":[{"role":"user","content":[{"type":"text","text":"a \\" quoted"}]},{"role":"assistant","content":[{"type":"tool_use","input":{"nested":[1,2,3]}}]}],"turn":1}"#;
        let ranges = locate_message_ranges(value).expect("ranges");
        assert_eq!(ranges.len(), 2);
        for (start, end) in ranges {
            let _: Value = serde_json::from_slice(&value[start as usize..end as usize])
                .expect("message slice remains valid json");
        }
    }

    /// Manual benchmark corpus used by the acceptance ledger. It deliberately has no wall-clock
    /// assertion; thresholds depend on calibrated hardware.
    #[test]
    #[ignore = "P4 benchmark fixture: run explicitly with --ignored --nocapture"]
    fn benchmark_10_1000_10000_session_fixtures() {
        for count in [10_usize, 1_000, 10_000] {
            let repo = crate::tempdir().expect("repo");
            let root = repo.path().join(".medusa/sessions");
            for index in 0..count {
                write_session(
                    &root,
                    &format!("session-{index:05}"),
                    &format!("2026-01-01T00:{:02}:{:02}Z", (index / 60) % 60, index % 60),
                    1,
                );
            }
            let repo_text = repo.path().to_string_lossy();
            let cold = std::time::Instant::now();
            let first = list_sessions_page_sync(&repo_text, None, 50).expect("cold page");
            let cold_elapsed = cold.elapsed();
            let warm = std::time::Instant::now();
            let second = list_sessions_page_sync(&repo_text, None, 50).expect("warm page");
            let warm_elapsed = warm.elapsed();
            assert_eq!(first.sessions.len(), 50.min(count));
            assert_eq!(second.sessions.len(), 50.min(count));
            eprintln!(
                "P4 sessions={count} cold_ms={} warm_ms={}",
                cold_elapsed.as_millis(),
                warm_elapsed.as_millis()
            );
        }
    }
}
