#!/usr/bin/env python3
"""Fail closed when model-executable production dispatch bypasses the certified pipeline."""

from __future__ import annotations

import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parents[1]
PIPELINE = ROOT / "crates/medusa-agent/src/tool_pipeline.rs"
TOOLS = ROOT / "crates/medusa-agent/src/tools/mod.rs"
ENGINE = ROOT / "crates/medusa-agent/src/engine.rs"
DOC = ROOT / "docs/TOOL-EXECUTION-PIPELINE.md"

REQUIRED_STAGES = [
    "Resolve",
    "PreExecute",
    "Guards",
    "Approval",
    "AroundDispatch",
    "Execute",
    "PostExecute",
    "Finalize",
    "Publish",
]


def fail(message: str) -> None:
    raise AssertionError(message)


def ordered(text: str, needles: list[str], label: str) -> None:
    cursor = 0
    for needle in needles:
        position = text.find(needle, cursor)
        if position < 0:
            fail(f"{label} is missing required item: {needle}")
        cursor = position + len(needle)


def main() -> int:
    pipeline = PIPELINE.read_text(encoding="utf-8")
    tools = TOOLS.read_text(encoding="utf-8")
    engine = ENGINE.read_text(encoding="utf-8")

    ordered(
        pipeline,
        [f"Self::{stage}" for stage in REQUIRED_STAGES],
        "certified pipeline stage order",
    )
    if "stages != ToolPipelineStage::BEFORE_FINALIZE" not in pipeline:
        fail("pipeline must fail closed on malformed stage order in release builds")
    monotonic_markers = [
        "let mut terminal_error = None;",
        "if terminal_error.is_some()",
        "GuardDecision::Deny(\"prior monotonic denial\".to_owned())",
        "if terminal_error.is_none()",
        "monotonic_denial_cannot_be_restored_by_later_guard",
    ]
    for marker in monotonic_markers:
        if marker not in pipeline:
            fail(f"pipeline must preserve monotonic denial: missing {marker}")
    ordered(
        tools,
        ['with_guard("capability_readiness"', 'with_guard("agent_execution_policy"'],
        "security guard order",
    )

    required_engine_entrypoints = [
        "execute_tool_cancellable_with_policy(",
        "execute_tool_cancellable_with_context_and_policy(",
        "execute_approved_tool_cancellable_with_policy(",
    ]
    for entrypoint in required_engine_entrypoints:
        if entrypoint not in engine:
            fail(f"engine is missing certified production entrypoint: {entrypoint}")

    forbidden_legacy_calls = [
        r"\bexecute_tool_cancellable\(",
        r"\bexecute_tool_cancellable_with_context\(",
        r"\bexecute_approved_tool_cancellable\(",
    ]
    for pattern in forbidden_legacy_calls:
        if re.search(pattern, engine):
            fail(f"engine contains legacy dispatch that bypasses active policy: {pattern}")

    if "let execution_policy = self.execution_policy.clone();" not in engine:
        fail("parallel tool-DAG dispatch must carry the active execution policy")
    if "ProviderStreamEvent::ToolUseReady" not in engine:
        fail("early streamed dispatch entrypoint disappeared")

    if not DOC.is_file():
        fail("certified tool pipeline architecture documentation is missing")
    documentation = DOC.read_text(encoding="utf-8")
    for authority in [
        "CapabilityRegistry",
        "AgentExecutionPolicy",
        "approval",
        "containment",
        "mutation",
        "verification",
        "durable session journal",
    ]:
        if authority not in documentation:
            fail(f"architecture documentation no longer names fixed authority: {authority}")

    print("certified-tool-pipeline-ok")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except AssertionError as error:
        print(f"certified-tool-pipeline-error: {error}", file=sys.stderr)
        raise SystemExit(1) from error
