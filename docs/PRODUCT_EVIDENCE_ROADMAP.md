# Product evidence roadmap

Medusa's product position is **auditable, recoverable autonomous engineering**: repository work should be bounded, verifiable, resumable, and explainable.

The repository already contains substantial runtime, containment, orchestration, verification, persistence, recovery, and learning infrastructure. The next milestone is to make those guarantees easy to reproduce, inspect, install, and measure.

## Product model

User-facing architecture should be expressed through three concepts:

1. **Plan** — understand the objective, repository state, dependencies, roles, and acceptance criteria.
2. **Execute Safely** — enforce repository, command, process, network, approval, transaction, and verification boundaries.
3. **Recover** — persist progress, classify failure, retry or replan within bounds, restore checkpoints, roll back unsafe changes, resume interrupted work, and explain the final outcome.

Multi-agent execution follows the same model. A primary agent may delegate bounded work to subagents when useful, but remains responsible for checking their evidence, resolving conflicts, and integrating only validated results.

## Delivery sequence

### Phase 1 — Establish the product contract

- [#392](https://github.com/benclawbot/Medusa/issues/392) — Add a cross-platform product acceptance suite for containment and recovery.
- [#394](https://github.com/benclawbot/Medusa/issues/394) — Add exportable session audit reports.
- [#393](https://github.com/benclawbot/Medusa/issues/393) — Publish a reproducible safety and recovery proof demo.

These items define the executable guarantees, evidence schema, and public demonstration. They should use the production runtime and remain consistent across TUI, desktop, and headless entrypoints.

### Phase 2 — Make the product legible and accessible

- [#395](https://github.com/benclawbot/Medusa/issues/395) — Collapse first run into one deterministic quickstart.
- [#396](https://github.com/benclawbot/Medusa/issues/396) — Document the product architecture as Plan, Execute Safely, Recover.
- [#397](https://github.com/benclawbot/Medusa/issues/397) — Enforce production capability maturity in code and documentation.

This phase turns internal architecture into a clear product path while preventing experimental or platform-limited behavior from being presented as generally available.

### Phase 3 — Compete on measurable reliability

- [#398](https://github.com/benclawbot/Medusa/issues/398) — Publish evidence-based reliability and recovery benchmarks.
- [#399](https://github.com/benclawbot/Medusa/issues/399) — Harden distribution trust and simplify provider delivery.

Release evidence should emphasize verified completion, false-completion prevention, restart continuity, recovery success, rollback success, containment violations, required intervention, and repeated-run determinism.

## Release gate target

A future generally available release should demonstrate all of the following from a clean environment:

- installation through a supported artifact or documented source path;
- one deterministic quickstart command;
- provider and containment capability preflight;
- a bounded repository task through the production runtime;
- denied external filesystem and network actions;
- controlled process-tree termination;
- interruption and durable resume;
- failed verification followed by rollback or recovery;
- successful repository verification;
- an exportable Markdown and JSON audit report;
- checksums, SBOM, provenance, and platform-appropriate artifact signing.

## Implementation rules

- Use production entrypoints and authoritative persisted state rather than standalone simulations.
- Keep repository verification as the completion gate.
- Separate deterministic runtime evidence from optional live-model evaluation.
- Fail closed when a requested safety policy or platform backend cannot be enforced.
- Do not duplicate evidence collection across acceptance tests, reports, demos, and benchmarks; establish shared versioned schemas.
- Allow primary agents to delegate to bounded subagents where useful, while keeping the primary agent accountable for validation, conflict resolution, and final integration.
- Do not mark implementation PRs ready or merge them until all required repository checks pass.