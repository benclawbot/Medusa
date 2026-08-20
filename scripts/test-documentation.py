#!/usr/bin/env python3
"""Exercise cross-platform documentation inventory hashing."""

from __future__ import annotations

import importlib.util
import sys
import tempfile
from pathlib import Path


CHECK_PATH = Path(__file__).with_name("check-documentation.py")
sys.dont_write_bytecode = True


def load_checker():
    spec = importlib.util.spec_from_file_location("check_documentation", CHECK_PATH)
    if spec is None or spec.loader is None:
        raise RuntimeError(f"cannot load {CHECK_PATH}")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def main() -> int:
    checker = load_checker()
    with tempfile.TemporaryDirectory() as directory:
        root = Path(directory)
        lf = root / "lf.md"
        crlf = root / "crlf.md"
        lf.write_bytes(b"# Title\n\nReviewed documentation.\n")
        crlf.write_bytes(b"# Title\r\n\r\nReviewed documentation.\r\n")
        assert checker.document_sha256(lf) == checker.document_sha256(crlf)

        vendored = root / "skills" / "writing-skills" / "anthropic-best-practices.md"
        governed = root / "docs" / "guide.md"
        assert not checker.is_governed_markdown(root, vendored)
        assert checker.is_governed_markdown(root, governed)
    print("documentation-tests-ok")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
