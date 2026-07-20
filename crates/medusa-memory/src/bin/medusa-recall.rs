use std::{env, path::PathBuf};

use medusa_memory::{SessionSearchQuery, open_session_recall};

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

    let store = open_session_recall(&repo)?;
    match command.as_str() {
        "search" => {
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

fn join_set(values: &std::collections::BTreeSet<String>) -> String {
    if values.is_empty() {
        "none".to_owned()
    } else {
        values.iter().cloned().collect::<Vec<_>>().join(", ")
    }
}

fn usage() -> &'static str {
    "Usage:\n  medusa-recall [--repo PATH] [--json] search QUERY... [--limit N] [--tool NAME] [--outcome VALUE] [--from RFC3339] [--to RFC3339]\n  medusa-recall [--repo PATH] [--json] open SESSION [--around ORDINAL] [--radius N]\n  medusa-recall [--repo PATH] [--json] compare SESSION_A SESSION_B"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flags_and_values_are_removed() {
        let mut args = vec!["search".to_owned(), "--json".to_owned(), "--limit".to_owned(), "5".to_owned()];
        assert!(take_flag(&mut args, "--json"));
        assert_eq!(take_value(&mut args, "--limit").as_deref(), Some("5"));
        assert_eq!(args, vec!["search"]);
    }

    #[test]
    fn usage_names_all_commands() {
        assert!(usage().contains("search"));
        assert!(usage().contains("open"));
        assert!(usage().contains("compare"));
    }
}
