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
for name in ("artifact", "frontend", "server", "desktop"):
    filename = ROOT / ".github" / "scripts" / f"embedded_{name}.py"
    namespace = {"__name__": "__main__", "__file__": str(filename)}
    exec(compile(scripts[name], str(filename), "exec"), namespace)
