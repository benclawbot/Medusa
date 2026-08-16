from pathlib import Path
import re

path = Path("crates/medusa-runtime/src/multi_agent_coordinator.rs")
text = path.read_text()

# Model-worker test doubles must report the same preallocated session that was sealed into the
# persisted attempt binding; fabricated session labels are intentionally rejected by recovery.
pattern = re.compile(
    r"(?P<indent>^[ \t]*)delegation_attempt_fingerprint: request\.attempt\.fingerprint,\n"
    r"(?P=indent)session_id: [^\n]+,",
    re.MULTILINE,
)

def use_bound_session(match: re.Match[str]) -> str:
    indent = match.group("indent")
    return (
        f"{indent}delegation_attempt_fingerprint: request.attempt.fingerprint,\n"
        f"{indent}session_id: request.session_id.to_string(),"
    )

text, count = pattern.subn(use_bound_session, text)
if count != 3:
    raise SystemExit(f"expected 3 model-worker session constructors, found {count}")

# This counter remains useful for the repository-change call-count assertion, but its returned
# ordinal is no longer a session identity.
old = "            let sequence = calls.fetch_add(1, Ordering::SeqCst) + 1;"
new = "            calls.fetch_add(1, Ordering::SeqCst);"
if old not in text:
    raise SystemExit("repository-change sequence binding missing")
text = text.replace(old, new, 1)

path.write_text(text)
