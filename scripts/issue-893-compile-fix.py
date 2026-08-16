from pathlib import Path
import re

# The parallel aggregate is deterministic composition of already-authorized child workers; it
# opens no model session of its own, so it must not invent a delegation contract identity.
path = Path("crates/medusa-runtime/src/parallel_mutation_batch.rs")
text = path.read_text()
old = """        task_id,
        worker_id: staging.id.clone(),
        session_id: format!("parallel-{}", hash(&parallel.dag_fingerprint)),"""
new = """        task_id,
        worker_id: staging.id.clone(),
        delegation_contract_id: String::new(),
        delegation_contract_fingerprint: String::new(),
        delegation_attempt_fingerprint: String::new(),
        session_id: format!("parallel-{}", hash(&parallel.dag_fingerprint)),"""
if old not in text:
    raise SystemExit("parallel aggregate evidence constructor missing")
path.write_text(text.replace(old, new, 1))

# Test executors stand in for actual model workers. Preserve the real delegation identity passed
# in their WorkerRequest so the same recovery/evidence checks run in tests as in production.
path = Path("crates/medusa-runtime/src/multi_agent_coordinator.rs")
text = path.read_text()
pattern = re.compile(
    r"(?P<indent>^[ \t]*)context_fingerprint: request\.packet\.fingerprint,\n"
    r"(?P=indent)lease_epoch: 0,\n"
    r"(?P=indent)session_id:",
    re.MULTILINE,
)

def bind_request_authority(match: re.Match[str]) -> str:
    indent = match.group("indent")
    return (
        f"{indent}context_fingerprint: request.packet.fingerprint,\n"
        f"{indent}lease_epoch: 0,\n"
        f"{indent}delegation_contract_id: request.delegation.contract_id,\n"
        f"{indent}delegation_contract_fingerprint: request.delegation.fingerprint,\n"
        f"{indent}delegation_attempt_fingerprint: request.attempt.fingerprint,\n"
        f"{indent}session_id:"
    )

text, count = pattern.subn(bind_request_authority, text)
if count != 3:
    raise SystemExit(f"expected 3 model-worker test constructors, found {count}")
path.write_text(text)

# These mutating-coordinator fixtures are synthetic preflight dependency reports. They do not
# represent a model session being recovered by the read-only coordinator, so an empty identity is
# explicit and non-authoritative.
path = Path("crates/medusa-runtime/src/mutating_worker_coordinator_tests.rs")
text = path.read_text()
pattern = re.compile(
    r"(?P<indent>^[ \t]*)lease_epoch: 1,\n(?P=indent)session_id:",
    re.MULTILINE,
)

def mark_synthetic_preflight(match: re.Match[str]) -> str:
    indent = match.group("indent")
    return (
        f"{indent}lease_epoch: 1,\n"
        f"{indent}delegation_contract_id: String::new(),\n"
        f"{indent}delegation_contract_fingerprint: String::new(),\n"
        f"{indent}delegation_attempt_fingerprint: String::new(),\n"
        f"{indent}session_id:"
    )

text, count = pattern.subn(mark_synthetic_preflight, text)
if count != 2:
    raise SystemExit(f"expected 2 synthetic preflight constructors, found {count}")
path.write_text(text)
