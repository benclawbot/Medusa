#!/usr/bin/env python3
"""Small CLI client for tools/local_bridge.py."""

from __future__ import annotations

import argparse
import json
import os
import sys
import urllib.error
import urllib.request


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command")
    parser.add_argument("args", nargs="*")
    parser.add_argument("--url", default=os.environ.get("MEDUSA_BRIDGE_URL", "http://127.0.0.1:8765"))
    parser.add_argument("--token", default=os.environ.get("MEDUSA_BRIDGE_TOKEN"))
    options = parser.parse_args()
    if not options.token:
        parser.error("set MEDUSA_BRIDGE_TOKEN or pass --token")

    body = json.dumps({"command": options.command, "args": options.args}).encode()
    request = urllib.request.Request(
        options.url.rstrip("/") + "/v1/run",
        data=body,
        method="POST",
        headers={
            "Authorization": f"Bearer {options.token}",
            "Content-Type": "application/json",
        },
    )
    try:
        with urllib.request.urlopen(request, timeout=3700) as response:
            result = json.load(response)
    except urllib.error.HTTPError as exc:
        result = json.loads(exc.read().decode())
        print(json.dumps(result, indent=2), file=sys.stderr)
        return 1
    except urllib.error.URLError as exc:
        print(f"bridge unavailable: {exc}", file=sys.stderr)
        return 2

    if result.get("stdout"):
        print(result["stdout"], end="")
    if result.get("stderr"):
        print(result["stderr"], end="", file=sys.stderr)
    return int(result.get("returncode", 1))


if __name__ == "__main__":
    raise SystemExit(main())
