from pathlib import Path


def replace_once(source: str, old: str, new: str, label: str) -> str:
    if new in source:
        return source
    count = source.count(old)
    if count != 1:
        raise SystemExit(f"{label}: expected one target, found {count}")
    return source.replace(old, new, 1)


path = Path("crates/medusa-cli/src/main.rs")
source = path.read_text()
source = replace_once(
    source,
    "use medusa_agent::bootstrap;\n",
    "use medusa_agent::{bootstrap, session_browser::list_sessions};\n",
    "session discovery import",
)
source = replace_once(
    source,
    '''    checks.push(desktop_commander_check(
        repo,
        &DesktopCommanderSettings::from_env(),
    ));
''',
    '''    checks.push(desktop_commander_check(
        repo,
        &DesktopCommanderSettings::from_env(),
    ));
    checks.extend(runtime_durability_checks(repo));
''',
    "runtime doctor checks",
)
helper = r'''fn runtime_durability_checks(repo: &Path) -> Vec<DoctorCheck> {
    let sessions = match list_sessions(repo) {
        Ok(sessions) => sessions,
        Err(error) => {
            let detail = format!("durable session discovery failed: {error}");
            return vec![
                DoctorCheck {
                    name: "execution_journal",
                    ok: false,
                    detail: detail.clone(),
                },
                DoctorCheck {
                    name: "runtime_checkpoints",
                    ok: false,
                    detail,
                },
            ];
        }
    };

    let mut journal_cursor = 0_u64;
    let mut journal_errors = Vec::new();
    let mut replayed_sessions = 0_usize;
    for session in &sessions {
        match medusa_runtime::execution_history::inspect(repo, &session.id) {
            Ok(health) if health.replay.equivalent => {
                replayed_sessions = replayed_sessions.saturating_add(1);
                journal_cursor = journal_cursor.saturating_add(health.journal_cursor);
            }
            Ok(_) => journal_errors.push(format!(
                "session {} replay diverges from its materialized state",
                session.id
            )),
            Err(error) => journal_errors.push(format!("session {}: {error}", session.id)),
        }
    }
    let journal = DoctorCheck {
        name: "execution_journal",
        ok: journal_errors.is_empty(),
        detail: if journal_errors.is_empty() {
            if sessions.is_empty() {
                "no durable sessions found; journal storage is ready".to_owned()
            } else {
                format!(
                    "verified {replayed_sessions} session journal(s) through {journal_cursor} total event cursor(s)"
                )
            }
        } else {
            format!(
                "{} journal verification failure(s): {}",
                journal_errors.len(),
                journal_errors.join("; ")
            )
        },
    };

    let mut checkpoint_count = 0_usize;
    let mut sessions_without_checkpoint = 0_usize;
    let mut latest_cursor = None::<u64>;
    let mut checkpoint_errors = Vec::new();
    for session in &sessions {
        match medusa_runtime::checkpoint_store::list(repo, &session.id) {
            Ok(records) => {
                if records.is_empty() {
                    sessions_without_checkpoint = sessions_without_checkpoint.saturating_add(1);
                }
                checkpoint_count = checkpoint_count.saturating_add(records.len());
                if let Some(cursor) = records.last().map(|record| record.journal_cursor) {
                    latest_cursor = Some(latest_cursor.map_or(cursor, |current| current.max(cursor)));
                }
            }
            Err(error) => checkpoint_errors.push(format!("session {}: {error}", session.id)),
        }
    }
    let checkpoints = DoctorCheck {
        name: "runtime_checkpoints",
        ok: checkpoint_errors.is_empty(),
        detail: if checkpoint_errors.is_empty() {
            if sessions.is_empty() {
                "no sessions require checkpoint recovery".to_owned()
            } else if checkpoint_count == 0 {
                format!(
                    "{sessions_without_checkpoint} session(s) are journal-resumable; no safe-boundary checkpoint exists yet"
                )
            } else {
                format!(
                    "verified {checkpoint_count} checkpoint(s); latest recoverable cursor {}; {sessions_without_checkpoint} session(s) currently rely on journal resume",
                    latest_cursor.unwrap_or_default()
                )
            }
        } else {
            format!(
                "{} checkpoint verification failure(s): {}",
                checkpoint_errors.len(),
                checkpoint_errors.join("; ")
            )
        },
    };

    vec![journal, checkpoints]
}

'''
marker = "fn migrate(repo: &Path) -> MedusaResult<()> {\n"
if helper not in source:
    if source.count(marker) != 1:
        raise SystemExit("runtime doctor helper insertion target changed")
    source = source.replace(marker, helper + marker, 1)

source = replace_once(
    source,
    '''    use super::*;
    use clap::CommandFactory;
''',
    '''    use super::*;
    use clap::CommandFactory;
    use medusa_agent::AgentEngine;
    use medusa_config::Config;
    use medusa_core::MedusaResult;
    use medusa_provider::{ModelProvider, ModelRequest, ModelResponse};

    struct UnusedProvider;

    impl ModelProvider for UnusedProvider {
        fn complete(&self, _: &ModelRequest) -> MedusaResult<ModelResponse> {
            unreachable!("session creation does not call the provider")
        }
    }
''',
    "runtime doctor test imports",
)
tests = r'''    #[test]
    fn runtime_doctor_accepts_empty_repository_state() {
        let repository = tempfile::tempdir().expect("repository");
        let checks = runtime_durability_checks(repository.path());
        assert_eq!(checks.len(), 2);
        assert!(checks.iter().all(|check| check.ok));
    }

    #[test]
    fn runtime_doctor_verifies_a_canonical_session_journal() {
        let repository = tempfile::tempdir().expect("repository");
        AgentEngine::new(UnusedProvider, Config::default())
            .create_session(repository.path(), "Verify durable journal".to_owned())
            .expect("session");
        let checks = runtime_durability_checks(repository.path());
        assert!(
            checks
                .iter()
                .find(|check| check.name == "execution_journal")
                .is_some_and(|check| check.ok && check.detail.contains("verified 1 session"))
        );
    }

    #[test]
    fn runtime_doctor_reports_corrupt_checkpoint_artifacts() {
        let repository = tempfile::tempdir().expect("repository");
        let session = AgentEngine::new(UnusedProvider, Config::default())
            .create_session(repository.path(), "Detect corrupt checkpoint".to_owned())
            .expect("session");
        let directory = repository
            .path()
            .join(".medusa/checkpoints")
            .join(session.id.as_str());
        fs::create_dir_all(&directory).expect("checkpoint directory");
        fs::write(directory.join("corrupt.json"), b"not-json").expect("corrupt checkpoint");

        let checks = runtime_durability_checks(repository.path());
        let checkpoint = checks
            .iter()
            .find(|check| check.name == "runtime_checkpoints")
            .expect("checkpoint check");
        assert!(!checkpoint.ok);
        assert!(checkpoint.detail.contains("checkpoint verification failure"));
    }

'''
test_marker = '''    #[test]
    fn hidden_daemon_host_accepts_repository_after_subcommand() {
'''
if tests not in source:
    if source.count(test_marker) != 1:
        raise SystemExit("runtime doctor tests insertion target changed")
    source = source.replace(test_marker, tests + test_marker, 1)
path.write_text(source)
