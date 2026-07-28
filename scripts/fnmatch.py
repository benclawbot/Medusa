"""Temporary stdlib-compatible shim used to extract the issue #474 patched source."""

from __future__ import annotations

import re


def translate(pattern: str) -> str:
    pieces: list[str] = []
    for character in pattern:
        if character == "*":
            pieces.append(".*")
        elif character == "?":
            pieces.append(".")
        else:
            pieces.append(re.escape(character))
    return f"(?s:{''.join(pieces)})\\Z"


def fnmatchcase(name: str, pattern: str) -> bool:
    return re.fullmatch(translate(pattern), name) is not None


import sitecustomize  # noqa: E402,F401 - registers the extraction hook after compatibility API exists
