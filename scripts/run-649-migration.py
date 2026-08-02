from __future__ import annotations

import subprocess
import sys
from pathlib import Path


def replace_once(text: str, before: str, after: str, label: str) -> str:
    count = text.count(before)
    if count != 1:
        raise RuntimeError(f"{label}: expected exactly one match, found {count}")
    return text.replace(before, after, 1)


# Normalize the one-shot generator before applying it.
generator = Path("scripts/apply-649-transaction.py")
text = generator.read_text(encoding="utf-8")
global_rewrite = '''text = coordinator.read_text(encoding="utf-8")
text = text.replace("            integration: None,\\n", "            transaction_path: root.join(\\\"mutation-transaction.json\\\"),\\n            legacy_integration: None,\\n")
text = text.replace("            integration: None,\\n", "            transaction_path: root.join(\\\"mutation-transaction.json\\\"),\\n            legacy_integration: None,\\n")
coordinator.write_text(text, encoding="utf-8")
'''
text = replace_once(
    text,
    global_rewrite,
    "",
    "ambiguous global integration initializer rewrite",
)
text = text.replace(
    '"use medusa_workers::{IntegrationReceipt, Worker, WorkerManager, WorkerState};\\n",\n    "use medusa_workers::{IntegrationReceipt, Worker, WorkerManager, WorkerState};\\n",',
    '"use medusa_workers::{IntegrationReceipt, Worker, WorkerManager, WorkerState};\\n",\n    "use medusa_workers::{Worker, WorkerManager};\\n",',
    1,
)
text = text.replace(
    '''    #[serde(default)]
    transaction_path: PathBuf,
    #[serde(default)]
    legacy_integration: Option<IntegrationReceipt>,
    last_error: Option<String>,
''',
    '''    #[serde(default)]
    transaction_path: PathBuf,
    last_error: Option<String>,
''',
    1,
)
generator.write_text(text, encoding="utf-8")
subprocess.run([sys.executable, str(generator)], check=True)

# Remove any remaining legacy symbols from the generated coordinator.
coordinator = Path("crates/medusa-runtime/src/mutating_worker_coordinator.rs")
text = coordinator.read_text(encoding="utf-8")
for before in (
    "use medusa_workers::{Worker, WorkerManager, WorkerState};\n",
    "use medusa_workers::{IntegrationReceipt, Worker, WorkerManager, WorkerState};\n",
):
    text = text.replace(before, "use medusa_workers::{Worker, WorkerManager};\n")
text = text.replace("            legacy_integration: None,\n", "")
if "WorkerState" in text or "legacy_integration" in text:
    raise RuntimeError("generated coordinator still contains removed legacy transaction symbols")
coordinator.write_text(text, encoding="utf-8")

# Delete the obsolete post-integration verification path.
multi = Path("crates/medusa-runtime/src/multi_agent_coordinator.rs")
text = multi.read_text(encoding="utf-8")
text = replace_once(
    text,
    "    TeamRuntime, WorkerExecutionController, targeted_verification,\n",
    "    TeamRuntime, WorkerExecutionController,\n",
    "obsolete repository verification import",
)
start = text.find("pub fn verify_repository(\n")
end = text.find("\nfn coordinate_with_control", start)
if start < 0 or end < 0:
    raise RuntimeError("obsolete repository verification function boundaries were not found")
text = text[:start] + text[end + 1 :]
if "verify_repository(" in text or "targeted_verification" in text:
    raise RuntimeError("obsolete post-integration verification path remains")
multi.write_text(text, encoding="utf-8")

# The integrate-before-review fixture is now replaced by a positive ordering gate.
checker = Path("scripts/check-architecture-index.py")
text = checker.read_text(encoding="utf-8")
text = replace_once(
    text,
    '    "integration-precedes-parent-review",\n',
    "",
    "resolved integration fixture requirement",
)
checker.write_text(text, encoding="utf-8")
