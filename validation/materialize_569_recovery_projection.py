from pathlib import Path

runtime_path = Path("crates/medusa-runtime/src/lib.rs")
source = runtime_path.read_text()
if "mod recovery_projection;" not in source:
    old = "mod recovery_tui;\nmod tool_policy;\n"
    new = "mod recovery_projection;\nmod recovery_tui;\nmod tool_policy;\n"
    if source.count(old) != 1:
        raise SystemExit("recovery projection module insertion target changed")
    source = source.replace(old, new, 1)
runtime_path.write_text(source)

checkpoint_path = Path("crates/medusa-runtime/src/checkpoint_store.rs")
checkpoint = checkpoint_path.read_text()
old = '''        let checkpoint = crate::checkpoint_store::materialize(repo, session_id)?;
        let checkpoint_id = checkpoint.checkpoint.fingerprint;
        let mut session = medusa_agent::session_browser::load_session(repo, session_id)
'''
new = '''        let checkpoint = crate::checkpoint_store::materialize(repo, session_id)?;
        let checkpoint_id = checkpoint.checkpoint.fingerprint;
        crate::recovery_projection::refresh(repo, session_id)?;
        let mut session = medusa_agent::session_browser::load_session(repo, session_id)
'''
if new not in checkpoint:
    if checkpoint.count(old) != 1:
        raise SystemExit("checkpoint recovery projection target changed")
    checkpoint = checkpoint.replace(old, new, 1)
checkpoint_path.write_text(checkpoint)
