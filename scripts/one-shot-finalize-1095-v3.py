#!/usr/bin/env python3
from pathlib import Path

claims = Path("docs/CAPABILITY-CLAIMS.json")
text = claims.read_text(encoding="utf-8")
old = "crates/medusa-runtime/src/mutating_worker_coordinator_tests.rs"
new = "crates/medusa-runtime/src/coordination/mutating_worker_coordinator_tests.rs"
if old not in text:
    raise SystemExit(f"capability claim test path not found: {old}")
claims.write_text(text.replace(old, new), encoding="utf-8")

architecture = Path("scripts/check-product-architecture.py")
text = architecture.read_text(encoding="utf-8")
old = "medusa-runtime::RuntimeController -> run_prompt -> multi_agent_coordinator::run_preflight -> mutating_worker_coordinator::run_implementation when required -> workspace-isolated candidate verification -> dedicated durable parent reviewer -> independent verification -> authorization -> integration -> reconciliation -> canonical terminal persistence"
new = "medusa-runtime::RuntimeController -> run_prompt -> coordination::multi_agent_coordinator::run_preflight -> coordination::mutating_worker_coordinator::run_implementation when required -> workspace-isolated candidate verification -> dedicated durable parent reviewer -> independent verification -> authorization -> integration -> reconciliation -> canonical terminal persistence"
if old not in text:
    raise SystemExit("stale production_entrypoint architecture authority not found")
architecture.write_text(text.replace(old, new, 1), encoding="utf-8")
