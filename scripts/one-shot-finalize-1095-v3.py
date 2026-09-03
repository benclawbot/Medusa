#!/usr/bin/env python3
from pathlib import Path

claims = Path("docs/CAPABILITY-CLAIMS.json")
text = claims.read_text(encoding="utf-8")
old = "crates/medusa-runtime/src/mutating_worker_coordinator_tests.rs"
new = "crates/medusa-runtime/src/coordination/mutating_worker_coordinator_tests.rs"
if old not in text:
    raise SystemExit(f"capability claim test path not found: {old}")
text = text.replace(old, new)
old_entrypoint = "medusa-runtime::RuntimeController -> run_prompt -> multi_agent_coordinator::run_preflight -> mutating_worker_coordinator::run_implementation"
new_entrypoint = "medusa-runtime::RuntimeController -> run_prompt -> coordination::multi_agent_coordinator::run_preflight -> coordination::mutating_worker_coordinator::run_implementation"
if old_entrypoint in text:
    text = text.replace(old_entrypoint, new_entrypoint, 1)
claims.write_text(text, encoding="utf-8")

architecture = Path("scripts/check-product-architecture.py")
text = architecture.read_text(encoding="utf-8")
old = "medusa-runtime::RuntimeController -> run_prompt -> multi_agent_coordinator::run_preflight -> mutating_worker_coordinator::run_implementation when required -> workspace-isolated candidate verification -> dedicated durable parent reviewer -> independent verification -> authorization -> integration -> reconciliation -> canonical terminal persistence"
new = "medusa-runtime::RuntimeController -> run_prompt -> coordination::multi_agent_coordinator::run_preflight -> coordination::mutating_worker_coordinator::run_implementation when required -> workspace-isolated candidate verification -> dedicated durable parent reviewer -> independent verification -> authorization -> integration -> reconciliation -> canonical terminal persistence"
if old not in text:
    raise SystemExit("stale production_entrypoint architecture authority not found")
text = text.replace(old, new, 1)
old_mutating = '    mutating_coordinator = read(root, "crates/medusa-runtime/src/coordination/mutating_worker_coordinator.rs")\n'
new_mutating = '''    mutating_coordinator = read(root, "crates/medusa-runtime/src/coordination/mutating_worker_coordinator.rs")
    mutating_coordinator += "\\n" + read(root, "crates/medusa-runtime/src/coordination/mutating_worker_coordinator_inner.rs")
    mutating_coordinator += "\\n" + read(root, "crates/medusa-runtime/src/coordination/mutating_worker_coordinator_support.rs")
'''
if old_mutating not in text:
    raise SystemExit("mutating coordinator wrapper read site not found")
text = text.replace(old_mutating, new_mutating, 1)
architecture.write_text(text, encoding="utf-8")

certified = Path("scripts/check-certified-tool-pipeline.py")
text = certified.read_text(encoding="utf-8")
old = '''    engine = ENGINE.read_text(encoding="utf-8")
    if 'include!("engine_inner.rs");' in engine:
        engine += "\\n" + ENGINE_INNER.read_text(encoding="utf-8")
'''
new = '''    engine = ENGINE.read_text(encoding="utf-8")
    if ENGINE_INNER.is_file():
        engine += "\\n" + ENGINE_INNER.read_text(encoding="utf-8")
'''
if old not in text:
    raise SystemExit("certified tool engine split read site not found")
certified.write_text(text.replace(old, new, 1), encoding="utf-8")

update = Path("crates/medusa-cli/src/update_command.rs")
text = update.read_text(encoding="utf-8")
old = '''        assert!(DEFAULT_PREBUILT_WAIT_SECS >= 60);
        assert!(PREBUILT_POLL_INTERVAL_SECS >= 5);
'''
new = '''        assert!(std::hint::black_box(DEFAULT_PREBUILT_WAIT_SECS) >= 60);
        assert!(std::hint::black_box(PREBUILT_POLL_INTERVAL_SECS) >= 5);
'''
if old not in text:
    raise SystemExit("updater constant default assertions not found")
update.write_text(text.replace(old, new, 1), encoding="utf-8")
