#!/usr/bin/env python3
"""Fail closed when model-executable production dispatch bypasses the certified pipeline."""

from __future__ import annotations

import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parents[1]
PIPELINE = ROOT / "crates/medusa-agent/src/tool_pipeline.rs"
TOOLS = ROOT / "crates/medusa-agent/src/tools/mod.rs"
ENGINE = ROOT / "crates/medusa-agent/src/engine_inner.rs"
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
        "execute_tool_cancellable_with_policy_certified(",
        "execute_tool_cancellable_with_context_and_policy_certified(",
        "execute_approved_tool_cancellable_with_policy_certified(",
        "execute_engine_tool_with_policy(",
        "certify_cached_tool_with_policy(",
    ]
    for entrypoint in required_engine_entrypoints:
        if entrypoint not in engine:
            fail(f"engine is missing receipt-preserving certified entrypoint: {entrypoint}")

    forbidden_legacy_calls = [
        r"\bexecute_tool_cancellable\(",
        r"\bexecute_tool_cancellable_with_context\(",
        r"\bexecute_approved_tool_cancellable\(",
        r"\bexecute_tool_cancellable_with_policy\(",
        r"\bexecute_tool_cancellable_with_context_and_policy\(",
        r"\bexecute_approved_tool_cancellable_with_policy\(",
    ]
    for pattern in forbidden_legacy_calls:
        if re.search(pattern, engine):
            fail(f"engine contains legacy dispatch that bypasses immutable receipts: {pattern}")

    if "let execution_policy = self.execution_policy.clone();" not in engine:
        fail("parallel tool-DAG dispatch must carry the active execution policy")
    if "ProviderStreamEvent::ToolUseReady" not in engine:
        fail("early streamed dispatch entrypoint disappeared")
    if "execution: CertifiedToolExecution" not in engine:
        fail("early streamed execution must retain the immutable certified execution")

    publication_markers = [
        "fn journal_certified_tool_execution(",
        "EventPayload::WorkerEvidenceRecorded",
        '"kind": "certified_tool_execution"',
        '"canonical_result": canonical.durable_evidence_projection()',
        '"execution_authority": execution_policy.audit_projection()',
        "persist(session)",
    ]
    for marker in publication_markers:
        if marker not in engine:
            fail(f"durable certified publication contract is missing: {marker}")
    central_journal_match = re.search(
        r"journal_certified_tool_execution\(\s*session,\s*&id,\s*&name,\s*&input,\s*receipt,\s*&result,\s*&self\.execution_policy,\s*\)\?;",
        engine,
    )
    if central_journal_match is None:
        fail(
            "durable certified publication contract must carry canonical result and active execution authority"
        )
    central_journal = central_journal_match.start()
    completion = engine.find("EventPayload::ToolExecutionCompleted", central_journal)
    frontend = engine.find("observer(&AgentUpdate::ToolOutput", central_journal)
    model_projection = engine.find("MessageBlock::ToolResult", central_journal)
    if min(completion, frontend, model_projection) < 0:
        fail("could not prove durable receipt publication before projections")
    if not (central_journal < completion < frontend < model_projection):
        fail("receipt must be journaled before completion/frontend/model publication")

    for pseudo_tool in [
        "ANALYSIS_WORKSPACE_TOOL",
        'name == "update_plan"',
        'name == "ask_user_question"',
        'name == "desktop_commander"',
        ".handles(&name)",
    ]:
        if pseudo_tool not in engine:
            fail(f"engine-owned executable path disappeared: {pseudo_tool}")

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
