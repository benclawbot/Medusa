from pathlib import Path

path = Path("crates/medusa-agent/src/engine/effective_request.rs")
text = path.read_text()

old = '''        execution_policy_fingerprint: &manifest.execution_policy_fingerprint,
        assembly_provenance: &manifest.assembly_provenance,'''
new = '''        execution_policy_fingerprint: &manifest.execution_policy_fingerprint,
        agent_scope_id: &manifest.agent_scope_id,
        agent_scope_fingerprint: &manifest.agent_scope_fingerprint,
        agent_scope_generation: manifest.agent_scope_generation,
        assembly_provenance: &manifest.assembly_provenance,'''
if old not in text:
    raise SystemExit("request replay scope fingerprint anchor missing")
text = text.replace(old, new, 1)

old = '''        session_id: &manifest.session_id,
        started_event_sequence: manifest.started_event_sequence,'''
new = '''        session_id: &manifest.session_id,
        agent_scope_id: &manifest.agent_scope_id,
        agent_scope_fingerprint: &manifest.agent_scope_fingerprint,
        agent_scope_generation: manifest.agent_scope_generation,
        started_event_sequence: manifest.started_event_sequence,'''
if old not in text:
    raise SystemExit("manifest replay scope fingerprint anchor missing")
text = text.replace(old, new, 1)

path.write_text(text)
