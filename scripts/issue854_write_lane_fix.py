from pathlib import Path

path = Path('crates/medusa-agent/src/engine.rs')
text = path.read_text()
old = '''fn execute_session_tool(
    repo: &Path,
    name: &str,
    input: &serde_json::Value,
    cancellation: &AtomicBool,
    session_id: &str,
    task_step_id: Option<&str>,
    activity_id: &str,
) -> MedusaResult<String> {
    if name != "fs_write" {
        return execute_tool_cancellable(repo, name, input, cancellation);
    }
    let sequence = crate::transaction::next_mutation_sequence(repo, session_id)?;
    let occurred_at_unix_ms = i64::try_from(
        OffsetDateTime::now_utc().unix_timestamp_nanos() / 1_000_000,
    )
    .map_err(|_| {
        MedusaError::new(
            ErrorCode::InternalInvariant,
            ErrorCategory::Internal,
            "mutation timestamp overflow",
        )
    })?;
    let context = crate::transaction::MutationContext {
        session_id: session_id.to_owned(),
        task_step_id: task_step_id.map(str::to_owned),
        activity_id: activity_id.to_owned(),
        actor: "medusa-agent".to_owned(),
        sequence,
        occurred_at_unix_ms,
    };
    execute_tool_cancellable_with_context(repo, name, input, cancellation, Some(&context))
}
'''
new = '''fn execute_session_tool(
    repo: &Path,
    name: &str,
    input: &serde_json::Value,
    cancellation: &AtomicBool,
    session_id: &str,
    task_step_id: Option<&str>,
    activity_id: &str,
) -> MedusaResult<String> {
    if name != "fs_write" {
        return execute_tool_cancellable(repo, name, input, cancellation);
    }

    let requested_path = input
        .get("path")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| MedusaError::new(
            ErrorCode::InvalidConfiguration,
            ErrorCategory::Validation,
            "fs_write path must be a string",
        ))?;

    // Absolute/external writes must reach the existing path-policy and approval boundary before
    // any provenance work. They are not repository mutations and cannot be selectively reverted.
    if Path::new(requested_path).is_absolute() {
        return execute_tool_cancellable(repo, name, input, cancellation);
    }

    // Non-Git workspaces remain writable, but repository-diff provenance is unavailable there.
    // Keep that limitation explicit instead of failing the write or manufacturing authority.
    let provenance_available = std::process::Command::new("git")
        .args(["diff", "--binary", "--no-ext-diff", "--", "."])
        .current_dir(repo)
        .output()
        .is_ok_and(|output| output.status.success());
    if !provenance_available {
        let output = execute_tool_cancellable(repo, name, input, cancellation)?;
        return Ok(format!(
            "{output}; selective_revert=unavailable (workspace has no authoritative Git provenance)"
        ));
    }

    let sequence = crate::transaction::next_mutation_sequence(repo, session_id)?;
    let occurred_at_unix_ms = i64::try_from(
        OffsetDateTime::now_utc().unix_timestamp_nanos() / 1_000_000,
    )
    .map_err(|_| {
        MedusaError::new(
            ErrorCode::InternalInvariant,
            ErrorCategory::Internal,
            "mutation timestamp overflow",
        )
    })?;
    let context = crate::transaction::MutationContext {
        session_id: session_id.to_owned(),
        task_step_id: task_step_id.map(str::to_owned),
        activity_id: activity_id.to_owned(),
        actor: "medusa-agent".to_owned(),
        sequence,
        occurred_at_unix_ms,
    };
    execute_tool_cancellable_with_context(repo, name, input, cancellation, Some(&context))
}
'''
if old not in text:
    raise SystemExit('missing execute_session_tool anchor')
path.write_text(text.replace(old, new, 1))
