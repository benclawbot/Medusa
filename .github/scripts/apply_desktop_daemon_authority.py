#!/usr/bin/env python3
import base64
import json
import zlib
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
payload = "".join(
    (ROOT / ".github" / "scripts" / f"desktop_authority_payload_{index}.txt").read_text()
    for index in range(5)
)
scripts = json.loads(zlib.decompress(base64.b64decode(payload)))

stale_desktop_row = (
    "| Desktop | `apps/medusa-desktop` | React/Tauri application | "
    "`medusa-runtime::RuntimeController` |"
)
current_desktop_row = (
    "| Desktop | `apps/medusa-desktop` | React/Tauri application | "
    "runtime command compatibility; canonical journal → `medusa-protocol` desktop projection |"
)
if scripts["desktop"].count(stale_desktop_row) != 1:
    raise SystemExit("embedded desktop generator no longer contains the expected stale architecture row")
scripts["desktop"] = scripts["desktop"].replace(
    stale_desktop_row,
    current_desktop_row,
    1,
)

for name in ("artifact", "frontend", "server", "desktop"):
    filename = ROOT / ".github" / "scripts" / f"embedded_{name}.py"
    namespace = {"__name__": "__main__", "__file__": str(filename)}
    exec(compile(scripts[name], str(filename), "exec"), namespace)
