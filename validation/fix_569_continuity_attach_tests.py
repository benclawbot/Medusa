from pathlib import Path
import re

path = Path("crates/medusa-session-continuity/src/root.rs")
source = path.read_text()

pattern = re.compile(
    r"(AttachRequest\s*\{(?:(?!journal_cursor).)*?expected_revision:\s*[^,\n]+,\n)(\s*)occurred_at_unix_ms:",
    re.DOTALL,
)
source, count = pattern.subn(
    lambda match: (
        f"{match.group(1)}"
        f"{match.group(2)}journal_cursor: 0,\n"
        f"{match.group(2)}occurred_at_unix_ms:"
    ),
    source,
)
if count == 0:
    remaining = re.findall(
        r"AttachRequest\s*\{(?:(?!journal_cursor).)*?occurred_at_unix_ms:",
        source,
        re.DOTALL,
    )
    if remaining:
        raise SystemExit(
            f"continuity attach fixtures still omit journal_cursor: {len(remaining)}"
        )

path.write_text(source)
