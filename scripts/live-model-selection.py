#!/usr/bin/env python3
"""Shared model selection for targeted MiniMax live diagnostics."""

from __future__ import annotations

import json
import os
from pathlib import Path

OVERRIDE_ENV = "MEDUSA_LIVE_MODEL"
KNOWN_MINIMAX_MODELS = {
    "MiniMax-M3",
    "MiniMax-M2.7",
    "MiniMax-M2.7-highspeed",
    "MiniMax-M2.5",
}


def canonical_model() -> str:
    manifest = json.loads(
        (Path(__file__).resolve().parents[1] / "docs/provider-support.json").read_text(
            encoding="utf-8"
        )
    )
    primary = [
        provider
        for provider in manifest.get("providers", [])
        if provider.get("dogfood", {}).get("status") == "primary"
    ]
    if len(primary) != 1:
        raise RuntimeError("provider support manifest must declare exactly one primary dogfood route")
    model = str(primary[0].get("dogfood", {}).get("model", "")).strip()
    if not model:
        raise RuntimeError("primary dogfood route must declare a model")
    return model


def selected_model(environ: dict[str, str] | None = None) -> str:
    env = os.environ if environ is None else environ
    default = canonical_model()
    override = env.get(OVERRIDE_ENV, "").strip()
    if not override:
        return default
    if override not in KNOWN_MINIMAX_MODELS:
        allowed = ", ".join(sorted(KNOWN_MINIMAX_MODELS))
        raise RuntimeError(f"unsupported {OVERRIDE_ENV}={override!r}; expected one of: {allowed}")
    return override
