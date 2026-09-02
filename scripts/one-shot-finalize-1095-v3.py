#!/usr/bin/env python3
from pathlib import Path

path = Path("docs/CAPABILITY-CLAIMS.json")
text = path.read_text(encoding="utf-8")
old = "crates/medusa-runtime/src/mutating_worker_coordinator_tests.rs"
new = "crates/medusa-runtime/src/coordination/mutating_worker_coordinator_tests.rs"
if old not in text:
    raise SystemExit(f"capability claim test path not found: {old}")
path.write_text(text.replace(old, new), encoding="utf-8")
