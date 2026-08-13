# ADR 0008: Verified browser model actions

## Status

Accepted.

## Context

Architecture v2 deliberately withheld model-executable `browser_*` tools because the capability registry had no production dispatcher even though `medusa-browserd`, `medusa-browser-client`, Playwright bridging, public-network pinning, and UI-change browser verification already existed. Advertising those tools without a dispatcher would violate the capability registry contract; leaving every action quarantined after the dispatcher and verification substrate exist also understates real capability.

Browser interaction is security-sensitive. A model-authored boolean such as `verified=true` cannot become authority, arbitrary JavaScript evaluation must not be model-executable, and local-loopback permission must not widen into access to unrelated services.

## Decision

Medusa exposes bounded model browser actions only when runtime-owned readiness succeeds:

- browser execution is explicitly enabled;
- the configured `medusa-browserd` sidecar passes its executable readiness probe;
- Node.js is available for the Playwright bridge; and
- `MEDUSA_BROWSER_VERIFY_URL` identifies the Medusa-owned UI verification route.

The capability registry is the only model-tool projection authority. The production agent dispatcher owns a repository-and-route-scoped stateful `BrowserClient`. Model actions are serialized through that client and cleaned up by explicit close, error reset, or bounded inactivity.

The model may use `browser_navigate`, `browser_snapshot`, `browser_click`, `browser_fill`, `browser_press`, `browser_screenshot`, `browser_tabs`, `browser_close`, and `browser_ping`. `browser_navigate` always targets the configured verification route; the model cannot supply another URL. `browser_evaluate` remains internal to authoritative browser verification and is never projected to the model.

Browser tool schemas deny unknown fields. In addition, the dispatcher explicitly rejects model-supplied trust fields such as `verified`, `verification`, `trusted`, `trust`, and `authority`. Verification truth remains owned by the authoritative changed-component verification path and its receipts.

Loopback access is bound to the exact configured verification origin (scheme, host, and port). The browser proxy continues to pin validated public destinations and rejects other loopback/private destinations, so a page interaction or script cannot pivot from the verification page to an unrelated local service.

## Evidence

The capability has three layers of proof:

1. registry tests prove default quarantine, readiness gating, read-only exclusion, least-privilege schemas, and absence of model `browser_evaluate`;
2. agent tests prove trust metadata and model-selected navigation URLs fail closed; and
3. `Browser Dispatch Certification` drives the real production `ToolManager` through the real `medusa-browserd` and Playwright bridge against an isolated verification fixture on Linux, macOS, and Windows, covering ping, navigation, snapshot, fill, click, key press, screenshot, tabs, spoof rejection, and close.

## Consequences

Browser model actions are no longer a structurally quarantined capability when the trusted runtime prerequisites are present. Installations without those prerequisites continue to advertise no browser tools. This adds no second verification authority, does not expose arbitrary JavaScript evaluation, and does not weaken the browser network boundary.
