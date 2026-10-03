#!/usr/bin/env python3
"""Reject workflow YAML that GitHub would refuse to load.

GitHub fails a workflow run whose file contains a duplicate mapping key, and the
only symptom is "this run likely failed because of a workflow file issue" with no
jobs to inspect. PyYAML's default loader silently keeps the last value for a
duplicated key, so the mistake is invisible to every other check.

This is deliberately not a general YAML linter: it asserts exactly the property
that turns a green run into an undebuggable one.
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

import yaml


# `python3 - <<'PY' ... PY` blocks embedded in workflow steps are real code that
# GitHub will execute on a runner, where a syntax error only surfaces as a red
# step several minutes in.
# The `PY` terminator is indented inside a block scalar, so it must not be
# anchored to column zero.
EMBEDDED_PYTHON = re.compile(r"python3? - <<'PY'\n(.*?)\n[ \t]*PY[ \t]*\n", re.DOTALL)


class DuplicateKeyLoader(yaml.SafeLoader):
    """SafeLoader that refuses a repeated mapping key instead of overwriting it."""


def _construct_mapping(loader: DuplicateKeyLoader, node: yaml.MappingNode, deep: bool = False) -> dict:
    mapping: dict = {}
    for key_node, value_node in node.value:
        key = loader.construct_object(key_node, deep=deep)
        if key in mapping:
            raise yaml.constructor.ConstructorError(
                "while constructing a mapping",
                node.start_mark,
                f"duplicate key {key!r} (first defined at line {key_node.start_mark.line + 1})",
                key_node.start_mark,
            )
        mapping[key] = loader.construct_object(value_node, deep=deep)
    return mapping


DuplicateKeyLoader.add_constructor(
    yaml.resolver.BaseResolver.DEFAULT_MAPPING_TAG,
    _construct_mapping,
)


def _check_embedded_python(workflow: str, path: Path) -> None:
    for index, block in enumerate(EMBEDDED_PYTHON.finditer(workflow)):
        lines = block.group(1).splitlines()
        indent = min((len(line) - len(line.lstrip()) for line in lines if line.strip()), default=0)
        source = "\n".join(line[indent:] if line.strip() else line for line in lines)
        try:
            compile(source, f"{path}:heredoc{index + 1}", "exec")
        except SyntaxError as error:
            raise ValueError(
                f"embedded python heredoc {index + 1} does not compile: {error}"
            ) from error


def check(path: Path) -> None:
    text = path.read_text(encoding="utf-8")
    yaml.load(text, Loader=DuplicateKeyLoader)
    _check_embedded_python(text, path)


def main(argv: list[str]) -> int:
    paths = [Path(argument) for argument in argv[1:]]
    if not paths:
        root = Path(__file__).resolve().parents[1]
        paths = sorted((root / ".github" / "workflows").glob("*.y*ml"))
        paths += sorted((root / ".github" / "actions").glob("*/action.yml"))
    failures = 0
    for path in paths:
        try:
            check(path)
        except (yaml.YAMLError, ValueError) as error:
            print(f"workflow-yaml-error: {path}: {error}", file=sys.stderr)
            failures += 1
    if failures:
        print(f"workflow-yaml-error: {failures} file(s) would not load on GitHub", file=sys.stderr)
        return 2
    print(f"workflow-yaml-ok:{len(paths)}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
