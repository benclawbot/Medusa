# Issue 685 execution-lane acceptance

The localized low-risk performance contract requires deterministic selection of `fast_mutation` for resolved one-file high-confidence work, with at most one model request before the first edit and at most two successful-path model requests.

Any unresolved scope, ambiguity, security-sensitive work, migration/release work, public API risk, dependency changes, generated-file risk, repository-wide scope, multi-package scope, repeated failures, or confidence below 700/1000 fails closed to `full_orchestration`.

This tranche establishes the typed selection contract and frozen turn budgets. Cross-platform timing and production-entrypoint certification remain required before #685 may close.
