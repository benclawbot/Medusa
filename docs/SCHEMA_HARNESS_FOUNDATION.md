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
- restart-safe model loading through `AgentEngine::load_session_world_model`;
- bounded passive observations after completed tool calls;
- source-specific provenance for file reads, searches, and shell commands;
- public session mutation helpers for user observations, hypotheses, transitions, and experiments.

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

The API deliberately separates observations, hypotheses, predictions, experiment outcomes, and invariants. Hypotheses cannot cite unknown observations. Refuted hypotheses cannot be promoted to leading or supported state without first adding new evidence.

Coordinator code can use `world_model_session::record_user_observation`, `add_hypothesis`, `transition_hypothesis`, and `add_experiment`. Each successful mutation persists atomically and updates the revision stored on the durable session.

## Failure isolation and compatibility

World-model creation is additive. A model-storage failure does not prevent a session from being created; the session receives no model reference and the existing Medusa loop continues normally. The new `world_model` field uses `serde(default)`, so older session JSON loads with no model reference. Loading an explicitly referenced but corrupt model returns a structured Medusa error rather than silently discarding evidence.

Passive tool observation is best effort: failure to load or persist telemetry never changes the result of the tool call. Observation bodies are bounded before persistence to avoid unbounded model growth.

## Current boundary

The foundation records grounded evidence but does not yet let the model autonomously promote hypotheses or select experiments. That policy layer should require explicit predictions, bounded experiment budgets, and post-execution reconciliation before it influences source mutation.

## Validation

The implementation includes tests for persistence round trips, evidence-link preservation, fabricated observation rejection, invalid hypothesis promotion, durable session creation and restart loading, source mapping, observation bounds, and session-level hypothesis mutations.
