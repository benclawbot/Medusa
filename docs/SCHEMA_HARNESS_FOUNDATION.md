# Schema-inspired harness foundation

This change introduces the durable data layer for evidence-driven debugging without replacing Medusa's current agent loop.

## Storage

World models are stored under:

```text
.medusa/world-models/<session-id>/model.json
```

Writes use a temporary file followed by an atomic rename. Session bootstrap creates the world-model root while preserving the existing session format.

## Epistemic model

The `medusa-world-model` crate distinguishes:

- observations produced by tools or users;
- hypotheses linked to existing observations;
- predictions recorded on proposed experiments;
- experiment outcomes;
- repository invariants.

Hypotheses cannot cite unknown observation IDs. Refuted hypotheses cannot be promoted back to leading or supported without first adding new evidence and revising their state. Model files carry an explicit schema version and monotonically increasing revision.

## Public API

```rust
use medusa_world_model::{
    Experiment, ExperimentAction, ObservationSource, WorkspaceModel,
    create_for_session, load, persist,
};

let reference = create_for_session(repo, session_id, objective)?;
let mut model = load(repo, &reference)?;
let observation = model.record_observation(
    ObservationSource::TestRun {
        command: "cargo test cancellation".into(),
        exit_code: 1,
    },
    "the cancellation test timed out",
);
model.add_hypothesis(
    "the child process is not receiving cancellation",
    vec![observation],
)?;
model.add_experiment(Experiment::new(
    "does the child process receive the cancellation signal?",
    ExperimentAction::RunTest {
        command: "cargo test cancellation_stops_child_process".into(),
    },
))?;
persist(repo, &reference.relative_path, &model)?;
```

## Rollout

This is deliberately a foundation PR. It does not yet alter tool selection or source-mutation policy. Follow-up work should add:

1. passive tool-output observation adapters;
2. hypothesis and experiment protocol events;
3. prediction-before-execution gates;
4. adaptive activation for ambiguous debugging tasks;
5. change proposals and post-edit reconciliation;
6. TUI views for structured evidence, not private chain-of-thought.

## Compatibility

Existing session JSON remains unchanged. The new storage directory is additive, and the harness can be disabled simply by not creating a world model for a session.
