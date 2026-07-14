use medusa_agent::output_envelope::{EnvelopeConfig, OutputFormat, wrap};

fn cfg(root: &std::path::Path) -> EnvelopeConfig {
    EnvelopeConfig {
        head_bytes: 16,
        tail_bytes: 16,
        max_artifact_bytes: 4096,
        session_root: root.to_path_buf(),
    }
}

#[test]
fn small_body_round_trips_in_head() {
    let dir = tempfile::tempdir().unwrap();
    let env = wrap("shell_run", b"hello world", OutputFormat::Plain, &cfg(dir.path())).unwrap();
    assert_eq!(env.head, "hello world");
    assert_eq!(env.tail, "");
    assert_eq!(env.line_count, 1);
    assert_eq!(env.byte_count, 11);
    assert!(env.path.exists());
    assert_eq!(std::fs::read(&env.path).unwrap(), b"hello world");
}

#[test]
fn large_body_splits_head_and_tail() {
    let dir = tempfile::tempdir().unwrap();
    let body = (0..200).map(|i| format!("line {i}\n")).collect::<String>();
    let env = wrap("shell_run", body.as_bytes(), OutputFormat::Plain, &cfg(dir.path())).unwrap();
    assert!(env.head.starts_with("line 0\n"));
    assert!(env.tail.ends_with("line 199\n"));
    assert_eq!(env.line_count, 200);
    assert!(env.path.exists());
    let stored = std::fs::read_to_string(&env.path).unwrap();
    assert_eq!(stored, body);
}

#[test]
fn body_above_max_artifact_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let body = vec![b'x'; 8192];
    let err = wrap("shell_run", &body, OutputFormat::Plain, &cfg(dir.path())).unwrap_err();
    assert!(format!("{err}").contains("artifact limit"));
}

#[test]
fn utf8_boundaries_are_preserved() {
    let dir = tempfile::tempdir().unwrap();
    let body = "éééééééééééééééééééé".repeat(8);
    let env = wrap("web_fetch", body.as_bytes(), OutputFormat::Plain, &cfg(dir.path())).unwrap();
    assert!(env.head.chars().all(|c| c.is_alphabetic() || c == 'é'));
    assert!(env.tail.chars().all(|c| c.is_alphabetic() || c == 'é'));
}

#[test]
fn shell_output_helper_no_longer_truncates() {
    let mut stdout = Vec::new();
    for i in 0..2_000 {
        stdout.extend_from_slice(format!("line {i}\n").as_bytes());
    }
    let stderr = Vec::new();
    let lines = medusa_agent::tools::format_command_output("cargo", &["test"], &stdout, &stderr);
    assert!(lines.iter().any(|l| l.contains("line 0")));
    assert!(lines.iter().any(|l| l.contains("line 1999")));
    assert!(!lines.iter().any(|l| l.contains("[truncated]")));
}