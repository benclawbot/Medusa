# ADR 0003: One truthful capability and plugin registry

- Status: Accepted
- Date: 2026-08-02
- Issue: #647
- Supersedes: legacy capability probes, `docs/CAPABILITY-CLAIMS.json` as runtime authority, hard-coded model tool lists, and directory-only skill discovery

## Context

Medusa currently describes runtime abilities in several independent places: command probes in `medusa-capabilities`, model tool JSON and dispatch matches in `medusa-agent`, plugin metadata in `medusa-extensions`, CLI diagnostics, frontend labels, protocol payloads, and prose capability claims. These sources can disagree. In particular, browser tool definitions are exposed to the model while the production `execute_tool` dispatcher has no browser route.

A capability name or product phrase is not authority. Availability must be derived from a typed descriptor, an explicit handler and lifecycle owner, a readiness contract, least-privilege permissions, platform and dependency checks, and proof status.

## Decision

`medusa-capabilities` owns the versioned registry and is the only production authority for capability and tool advertisement.

Every registry entry contains:

- a stable identifier, kind, description, owner, lifecycle owner, and maturity status;
- the surfaces it may project to: model, CLI, UI, protocol, and documentation;
- dependencies, platform support, permissions, explicit-approval requirements, and readiness evidence;
- an optional model tool schema;
- an explicit production handler identifier when the entry is executable;
- plugin provenance and integrity metadata when supplied by a plugin.

Registry construction validates uniqueness and fails closed. An executable entry cannot project to a production surface without a registered handler and readiness contract. An unavailable dependency removes the entry from every executable surface in the same snapshot. Model tool definitions, prompt summaries, CLI diagnostics, protocol reports, and generated documentation are projections of that snapshot.

Browser actions remain absent from executable surfaces until the browser dispatcher is registered and passes readiness and behavioral conformance tests.

`medusa-extensions` owns managed plugin loading. A managed plugin manifest may declare instructions, tools, scripts, resources, MCP servers, authentication requirements, permissions, compatibility, and integrity provenance. `SKILL.md` remains supported as an instruction-only compatibility plugin; it cannot create executable authority merely by naming tools.

Free-text identity or brand phrases never grant capabilities, permissions, or provenance. Authority is structural and auditable through registry entry IDs and handler/plugin provenance.

## Consequences

- Adding a tool requires one registry descriptor and one registered handler rather than parallel model and dispatcher edits.
- Disabled, unauthenticated, unsupported, or unavailable dependencies disappear consistently from model, CLI, UI, and protocol projections.
- Plugins cannot activate tools without explicit permissions, authentication readiness, integrity evidence, and a production handler.
- Legacy claim files become generated migration artifacts and are removed once all production consumers use the registry.
- Conformance tests reject duplicate IDs, missing handlers, surface/dispatch divergence, and browser advertisement without executable dispatch.

## Migration

1. Introduce the versioned registry contract and strict validation.
2. Register built-in tools and capability groups with explicit handler IDs and readiness probes.
3. Generate agent model definitions and prompt capability summaries from the registry.
4. Generate CLI and protocol diagnostics from the same snapshot.
5. Introduce managed plugin manifests and adapt `SKILL.md` to instruction-only plugins.
6. Remove browser definitions until their dispatcher is certified.
7. Remove legacy runtime claims and update the living architecture index and baseline.
