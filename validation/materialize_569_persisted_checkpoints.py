from pathlib import Path

checkpoint_path = Path("crates/medusa-runtime/src/checkpoint_store.rs")
checkpoint = checkpoint_path.read_text()
checkpoint = checkpoint.replace(
    "    fs::{self, File, OpenOptions},\n",
    "    fs::{self, OpenOptions},\n",
)
if "#[cfg(unix)]\nuse std::fs::File;" not in checkpoint:
    checkpoint = checkpoint.replace(
        "use std::{\n",
        "#[cfg(unix)]\nuse std::fs::File;\n\nuse std::{\n",
        1,
    )
checkpoint_path.write_text(checkpoint)

runtime_path = Path("crates/medusa-runtime/src/lib.rs")
source = runtime_path.read_text()
if "pub mod checkpoint_store;" not in source:
    old = "pub mod attachment;\npub mod commands;\n"
    new = "pub mod attachment;\npub mod checkpoint_store;\npub mod commands;\n"
    if source.count(old) != 1:
        raise SystemExit("checkpoint module insertion target changed")
    source = source.replace(old, new, 1)

if "pub use checkpoint_store::RuntimeCheckpointRecord;" not in source:
    old = "pub use error::RuntimeError;\n"
    new = "pub use checkpoint_store::RuntimeCheckpointRecord;\npub use error::RuntimeError;\n"
    if source.count(old) != 1:
        raise SystemExit("checkpoint export insertion target changed")
    source = source.replace(old, new, 1)

old = '''fn record_controller_event(
    repo: &std::path::Path,
    session_id: &str,
    actor: Actor,
    payload: EventPayload,
) -> Result<(), RuntimeError> {
    let mut session = medusa_agent::session_browser::load_session(repo, session_id)
        .map_err(RuntimeError::agent)?;
    medusa_agent::record_session_event(&mut session, actor, payload).map_err(RuntimeError::agent)
}
'''
new = '''fn record_controller_event(
    repo: &std::path::Path,
    session_id: &str,
    actor: Actor,
    payload: EventPayload,
) -> Result<(), RuntimeError> {
    let checkpoint_boundary = crate::checkpoint_store::is_checkpoint_boundary(&payload);
    let mut session = medusa_agent::session_browser::load_session(repo, session_id)
        .map_err(RuntimeError::agent)?;
    medusa_agent::record_session_event(&mut session, actor, payload)
        .map_err(RuntimeError::agent)?;
    if checkpoint_boundary {
        let checkpoint = crate::checkpoint_store::materialize(repo, session_id)?;
        let checkpoint_id = checkpoint.checkpoint.fingerprint;
        let mut session = medusa_agent::session_browser::load_session(repo, session_id)
            .map_err(RuntimeError::agent)?;
        let already_recorded = session.events.last().is_some_and(|event| {
            matches!(
                &event.payload,
                EventPayload::CheckpointCreated {
                    checkpoint_id: existing,
                } if existing == &checkpoint_id
            )
        });
        if !already_recorded {
            medusa_agent::record_session_event(
                &mut session,
                Actor::Coordinator,
                EventPayload::CheckpointCreated { checkpoint_id },
            )
            .map_err(RuntimeError::agent)?;
        }
    }
    Ok(())
}
'''
if new not in source:
    if source.count(old) != 1:
        raise SystemExit("controller checkpoint wiring target changed")
    source = source.replace(old, new, 1)

runtime_path.write_text(source)
