# ADR-0009: Browser actions are readiness-gated preview capability

## Status

Accepted.

## Context

Browser action dispatch is implemented through `medusa-agent::ToolManager` and `medusa-browserd`, with cross-platform dispatcher and hardening certification. Older human-facing documentation still described browser actions as withheld or as having no dispatcher, contradicting the machine-readable architecture baseline and runtime registry.

The runtime deliberately does not make browser actions ambient or default. Model projection is admitted only when the browser feature is explicitly enabled and all runtime prerequisites are ready. `browser_evaluate` remains verifier-internal and is not projected to the model.

## Decision

Browser actions have **product status `preview`** and **architecture status `certified-production`**. In this terminology, `certified-production` means the dispatcher, permission boundary, supported entrypoint, conformance evidence, observability/recovery behavior, and trust controls are certified; `preview` means operators must explicitly opt in and the product surface is not default-enabled.

The canonical implementation path is:

`medusa-agent::ToolManager -> medusa-browser-client -> medusa-browserd -> Playwright`

Model browser actions are projected only when all of these conditions hold:

1. browser execution is explicitly enabled (`MEDUSA_BROWSER_ENABLED=true`);
2. an explicit sidecar path is configured (`MEDUSA_BROWSER_PATH`);
3. the sidecar readiness probe succeeds;
4. Node.js is available;
5. `MEDUSA_BROWSER_VERIFY_URL` is present and passes the browser verification-route admission policy.

The configured verification route is also the network authority for model browser navigation. The browser sidecar pins loopback access to that admitted origin rather than granting general localhost access. Model input cannot supply verification/trust metadata or choose an arbitrary navigation origin.

`MEDUSA_BROWSER_TIMEOUT_MS` controls the bounded browser request timeout. It must be a positive integer. Timeout does not weaken route admission, permission checks, output bounds, or cleanup.

Screenshots and other binary browser outputs use Medusa's bounded output-envelope/artifact path. Browser sessions are repository/route scoped and are explicitly closed or cleaned up after bounded inactivity.

## Certification boundary

This ADR does not convert browser actions into a default-enabled product feature and does not claim protections outside the certified dispatcher boundary. Open browser-hardening work remains independently tracked and must not be represented as complete merely because the preview dispatcher is certified.

Authoritative final verification remains separate from model browser interaction. A model browser action cannot author or replace a `VerificationReceipt`.

## Documentation rule

Human-facing documentation must use the exact status distinction above: **preview, readiness-gated, explicit opt-in; dispatcher certified; not default-enabled**. Documentation must not describe the capability as withheld, quarantined, or lacking a dispatcher while the machine authority remains in this state.

The architecture status guard cross-checks the baseline, this ADR, README status language, configuration prerequisites, and architecture index so a unilateral documentation revert fails CI.

## Consequences

- Operators can use the implemented browser actions when they deliberately satisfy the prerequisites.
- Default installations continue to expose no model browser tools until readiness is proven.
- Documentation and runtime authority have one status vocabulary.
- Future promotion from preview requires a separate architecture/product decision and evidence update.
