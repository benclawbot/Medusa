from pathlib import Path

path = Path("crates/medusa-agent/src/engine.rs")
text = path.read_text()
old = "        agent_runtime_handle, effective_agent_scope_tools, fail_agent_scope_start,\n"
new = "        agent_runtime_handle, effective_agent_scope_tools, fail_agent_scope_start,\n        load_published_scope_ref,\n"
if old not in text:
    raise SystemExit("agent scope import anchor missing")
path.write_text(text.replace(old, new, 1))
