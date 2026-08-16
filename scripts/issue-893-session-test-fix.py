from pathlib import Path
import re

path = Path("crates/medusa-runtime/src/multi_agent_coordinator.rs")
text = path.read_text()
marker = "#[cfg(test)]\nmod tests {"
if marker not in text:
    raise SystemExit("multi-agent coordinator test module missing")
production, tests = text.split(marker, 1)

# Production already reports session.id, which is the exact preallocated SessionId consumed by
# create_session_with_id. Only model-worker test doubles fabricate labels and need correction.
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

tests, count = pattern.subn(use_bound_session, tests)
if count != 3:
    raise SystemExit(f"expected 3 model-worker test session constructors, found {count}")

# This counter remains useful for the repository-change call-count assertion, but its returned
# ordinal is no longer a session identity.
old = "            let sequence = calls.fetch_add(1, Ordering::SeqCst) + 1;"
new = "            calls.fetch_add(1, Ordering::SeqCst);"
if old not in tests:
    raise SystemExit("repository-change sequence binding missing")
tests = tests.replace(old, new, 1)

path.write_text(production + marker + tests)
