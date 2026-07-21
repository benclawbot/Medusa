use std::{
    collections::BTreeSet,
    env,
    path::{Path, PathBuf},
};

use medusa_memory::{SessionSearchQuery, open_session_recall};
use rusqlite::Connection;
use serde::Serialize;

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct SessionListEntry {
    session_id: String,
    parent_session_id: Option<String>,
    created_at: String,
    repository_fingerprint: String,
    outcome: String,
    tools: BTreeSet<String>,
    event_count: usize,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct SessionListQuery {
    repository_fingerprint: Option<String>,
    date_from: Option<String>,
    date_to: Option<String>,
    tool: Option<String>,
    outcome: Option<String>,
    limit: usize,
}

fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = env::args().skip(1).collect::<Vec<_>>();
    let json = take_flag(&mut args, "--json");
    let repo = take_value(&mut args, "--repo")
        .map(PathBuf::from)
        .unwrap_or(env::current_dir()?);
    let Some(command) = args.first().cloned() else {
        return Err(usage().into());
    };
    args.remove(0);

    match command.as_str() {
        "list" => {
            let query = SessionListQuery {
                repository_fingerprint: take_value(&mut args, "--repository"),
                date_from: take_value(&mut args, "--from"),
                date_to: take_value(&mut args, "--to"),
                tool: take_value(&mut args, "--tool"),
                outcome: take_value(&mut args, "--outcome"),
                limit: take_value(&mut args, "--limit")
                    .map(|value| value.parse::<usize>())
                    .transpose()?
                    .unwrap_or(20),
            };
            reject_extra(&args)?;
            let entries = list_sessions(&repo, &query)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&entries)?);
            } else if entries.is_empty() {
                println!("No recorded sessions.");
            } else {
                for entry in entries {
                    println!(
                        "{}  {}  {}",
                        entry.session_id, entry.created_at, entry.outcome
                    );
                    println!(
                        "  repository={} events={} tools={}",
                        entry.repository_fingerprint,
                        entry.event_count,
                        join_set(&entry.tools)
                    );
                    if let Some(parent) = entry.parent_session_id {
                        println!("  parent={parent}");
                    }
                }
            }
        }
        "search" => {
            let store = open_session_recall(&repo)?;
            if args.is_empty() {
                return Err("search requires one or more query terms".into());
            }
            let limit = take_value(&mut args, "--limit")
                .map(|value| value.parse::<usize>())
                .transpose()?
                .unwrap_or(10);
            let tool = take_value(&mut args, "--tool");
            let outcome = take_value(&mut args, "--outcome");
            let date_from = take_value(&mut args, "--from");
            let date_to = take_value(&mut args, "--to");
            let query = args.join(" ");
            let hits = store.session_search(&SessionSearchQuery {
                query,
                repository_fingerprint: None,
                date_from,
                date_to,
                tool,
                outcome,
                limit,
            })?;
            if json {
                println!("{}", serde_json::to_string_pretty(&hits)?);
            } else if hits.is_empty() {
                println!("No matching sessions.");
            } else {
                for hit in hits {
                    println!("{}  {}  {}", hit.session_id, hit.created_at, hit.outcome);
                    println!("  {}", hit.excerpt.replace('\n', " "));
                }
            }
        }
        "open" => {
            let store = open_session_recall(&repo)?;
            let session_id = required(&mut args, "open requires a session id")?;
            let around = take_value(&mut args, "--around")
                .map(|value| value.parse::<usize>())
                .transpose()?;
            let radius = take_value(&mut args, "--radius")
                .map(|value| value.parse::<usize>())
                .transpose()?
                .unwrap_or(3);
            reject_extra(&args)?;
            let window = store.session_open(&session_id, around, radius)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&window)?);
            } else {
                println!("session {}", window.session_id);
                if let Some(parent) = window.parent_session_id {
                    println!("parent {parent}");
                }
                for event in window.events {
                    let tool = event
                        .tool
                        .as_deref()
                        .map(|value| format!(" tool={value}"))
                        .unwrap_or_default();
                    println!("[{}] {}{}", event.ordinal, event.kind, tool);
                    println!("  {}", event.text.replace('\n', " "));
                }
            }
        }
        "compare" => {
            let store = open_session_recall(&repo)?;
            let session_a = required(&mut args, "compare requires the first session id")?;
            let session_b = required(&mut args, "compare requires the second session id")?;
            reject_extra(&args)?;
            let comparison = store.session_compare(&session_a, &session_b)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&comparison)?);
            } else {
                println!("{} vs {}", comparison.session_a, comparison.session_b);
                println!("same repository: {}", comparison.same_repository);
                println!("same outcome: {}", comparison.same_outcome);
                println!("shared tools: {}", join_set(&comparison.shared_tools));
                println!("only first: {}", join_set(&comparison.only_a_tools));
                println!("only second: {}", join_set(&comparison.only_b_tools));
                println!(
                    "successful events: {} / {}",
                    comparison.successful_events_a, comparison.successful_events_b
                );
                println!(
                    "failed events: {} / {}",
                    comparison.failed_events_a, comparison.failed_events_b
                );
            }
        }
        "help" | "--help" | "-h" => println!("{}", usage()),
        _ => return Err(format!("unknown recall command: {command}\n{}", usage()).into()),
    }
    Ok(())
}

fn list_sessions(
    repo: &Path,
    query: &SessionListQuery,
) -> Result<Vec<SessionListEntry>, Box<dyn std::error::Error>> {
    let path = repo.join(".medusa/session-recall.sqlite3");
    if !path.exists() {
        return Ok(Vec::new());
    }
    let connection = Connection::open(path)?;
    let mut statement = connection.prepare(
        "SELECT session_id, parent_session_id, created_at, repository_fingerprint, \
                tools_json, outcome, events_json \
         FROM session_recall ORDER BY created_at DESC, session_id DESC",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, Option<String>>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, String>(4)?,
            row.get::<_, String>(5)?,
            row.get::<_, String>(6)?,
        ))
    })?;

    let limit = query.limit.clamp(1, 100);
    let mut entries = Vec::new();
    for row in rows {
        let (
            session_id,
            parent_session_id,
            created_at,
            repository,
            tools_json,
            outcome,
            events_json,
        ) = row?;
        let tools: BTreeSet<String> = serde_json::from_str(&tools_json)?;
        if query
            .repository_fingerprint
            .as_ref()
            .is_some_and(|value| value != &repository)
            || query
                .tool
                .as_ref()
                .is_some_and(|value| !tools.contains(value))
            || query
                .outcome
                .as_ref()
                .is_some_and(|value| value != &outcome)
            || query
                .date_from
                .as_ref()
                .is_some_and(|value| &created_at < value)
            || query
                .date_to
                .as_ref()
                .is_some_and(|value| &created_at > value)
        {
            continue;
        }
        let events: Vec<serde_json::Value> = serde_json::from_str(&events_json)?;
        entries.push(SessionListEntry {
            session_id,
            parent_session_id,
            created_at,
            repository_fingerprint: repository,
            outcome,
            tools,
            event_count: events.len(),
        });
        if entries.len() == limit {
            break;
        }
    }
    Ok(entries)
}

fn required(args: &mut Vec<String>, message: &str) -> Result<String, Box<dyn std::error::Error>> {
    if args.is_empty() || args[0].starts_with('-') {
        return Err(message.into());
    }
    Ok(args.remove(0))
}

fn take_flag(args: &mut Vec<String>, flag: &str) -> bool {
    if let Some(index) = args.iter().position(|value| value == flag) {
        args.remove(index);
        true
    } else {
        false
    }
}

fn take_value(args: &mut Vec<String>, flag: &str) -> Option<String> {
    let index = args.iter().position(|value| value == flag)?;
    args.remove(index);
    if index < args.len() {
        Some(args.remove(index))
    } else {
        None
    }
}

fn reject_extra(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    if args.is_empty() {
        Ok(())
    } else {
        Err(format!("unexpected arguments: {}", args.join(" ")).into())
    }
}

fn join_set(values: &BTreeSet<String>) -> String {
    if values.is_empty() {
        "none".to_owned()
    } else {
        values.iter().cloned().collect::<Vec<_>>().join(", ")
    }
}

fn usage() -> &'static str {
    "Usage:\n  medusa-recall [--repo PATH] [--json] list [--limit N] [--repository FINGERPRINT] [--tool NAME] [--outcome VALUE] [--from RFC3339] [--to RFC3339]\n  medusa-recall [--repo PATH] [--json] search QUERY... [--limit N] [--tool NAME] [--outcome VALUE] [--from RFC3339] [--to RFC3339]\n  medusa-recall [--repo PATH] [--json] open SESSION [--around ORDINAL] [--radius N]\n  medusa-recall [--repo PATH] [--json] compare SESSION_A SESSION_B"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flags_and_values_are_removed() {
        let mut args = vec![
            "search".to_owned(),
            "--json".to_owned(),
            "--limit".to_owned(),
            "5".to_owned(),
        ];
        assert!(take_flag(&mut args, "--json"));
        assert_eq!(take_value(&mut args, "--limit").as_deref(), Some("5"));
        assert_eq!(args, vec!["search"]);
    }

    #[test]
    fn list_filters_and_orders_sessions() {
        let directory = tempfile::tempdir().expect("tempdir");
        let medusa = directory.path().join(".medusa");
        std::fs::create_dir_all(&medusa).expect("directory");
        let connection = Connection::open(medusa.join("session-recall.sqlite3")).expect("db");
        connection
            .execute_batch(
                "CREATE VIRTUAL TABLE session_recall USING fts5(\
                   session_id UNINDEXED, parent_session_id UNINDEXED, created_at UNINDEXED,\
                   repository_fingerprint UNINDEXED, tools_json UNINDEXED, outcome UNINDEXED,\
                   events_json UNINDEXED, text\
                 );\
                 INSERT INTO session_recall VALUES\
                   ('old', NULL, '2026-07-19T10:00:00Z', 'repo-a', '[\"shell\"]', 'failure', '[{},{}]', 'old');\
                 INSERT INTO session_recall VALUES\
                   ('new', 'old', '2026-07-20T10:00:00Z', 'repo-a', '[\"shell\",\"git\"]', 'success', '[{},{},{}]', 'new');",
            )
            .expect("schema");
        drop(connection);

        let entries = list_sessions(
            directory.path(),
            &SessionListQuery {
                repository_fingerprint: Some("repo-a".to_owned()),
                tool: Some("git".to_owned()),
                outcome: Some("success".to_owned()),
                limit: 10,
                ..SessionListQuery::default()
            },
        )
        .expect("list");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].session_id, "new");
        assert_eq!(entries[0].parent_session_id.as_deref(), Some("old"));
        assert_eq!(entries[0].event_count, 3);
    }

    #[test]
    fn missing_recall_database_is_an_empty_history() {
        let directory = tempfile::tempdir().expect("tempdir");
        assert!(
            list_sessions(
                directory.path(),
                &SessionListQuery {
                    limit: 20,
                    ..SessionListQuery::default()
                }
            )
            .expect("list")
            .is_empty()
        );
    }

    #[test]
    fn usage_names_all_commands() {
        assert!(usage().contains("list"));
        assert!(usage().contains("search"));
        assert!(usage().contains("open"));
        assert!(usage().contains("compare"));
    }
}
