# Schema-inspired harness foundation

Medusa now has a durable, explicit world-model layer for evidence-driven engineering work.

## Implemented

- versioned workspace models;
- objective and acceptance-criteria state;
- direct observations with typed provenance;
- evidence-linked hypotheses;
- guarded hypothesis transitions;
- predictions recorded against hypotheses;
- structured experiments and outcomes;
- repository invariants;
- atomic persistence under `.medusa/world-models/<session-id>/model.json`;
- automatic world-model creation for new agent sessions;
- backward-compatible loading for sessions created before world models existed;
- restart-safe model loading through `AgentEngine::load_session_world_model`.

## Storage

Each model is stored separately from the main session JSON:

```text
.medusa/
├── sessions/
│   └── <session-id>.json
└── world-models/
    └── <session-id>/
        └── model.json
```

The session stores only a relative-path and revision reference. This keeps conversation persistence bounded while allowing the model schema to evolve independently.

## Epistemic rules

The API deliberately separates:

- observations, which come from users or tools;
- hypotheses, which are falsifiable explanations;
- predictions, which must exist before experiment reconciliation;
- experiment outcomes;
- invariants, which represent repository properties that must remain true.

Hypotheses cannot cite unknown observations. Refuted hypotheses cannot be promoted to leading or supported state without first adding new evidence.

## Failure isolation

World-model creation is additive. A model-storage failure does not prevent a session from being created; the session receives no model reference and the existing Medusa loop continues normally. Loading an explicitly referenced but corrupt model returns a structured Medusa error rather than silently discarding evidence.

## Current boundary

This layer does not yet change normal model tool selection. It supplies the durable substrate required for follow-up work:

1. passive tool-result observation adapters;
2. protocol events for model revisions;
3. hypothesis and experiment selection;
4. prediction-before-execution gates;
5. proposed-change and invariant verification gates;
6. compact TUI rendering.

## Validation

The crate includes tests for:

- persistence round trips;
- evidence-link preservation;
- fabricated observation rejection;
- invalid hypothesis promotion;
- durable session creation and restart loading.
