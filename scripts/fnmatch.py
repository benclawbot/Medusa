"""Temporary stdlib-compatible shim used to extract the issue #474 patched source."""

from __future__ import annotations

import re

import sitecustomize  # noqa: F401 - registers the extraction hook


def fnmatchcase(name: str, pattern: str) -> bool:
    translated = re.escape(pattern).replace(r"\*", ".*").replace(r"\?", ".")
    return re.fullmatch(translated, name) is not None
