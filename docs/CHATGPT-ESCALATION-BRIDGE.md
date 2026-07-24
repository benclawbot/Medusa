# ChatGPT escalation bridge

## Purpose

The ChatGPT escalation bridge lets Medusa keep the expensive autonomous loop local while reserving a stronger remote reasoning surface for the moments that materially benefit from it.

This is not a ChatGPT web-session scraper, browser automation workaround, or quota bypass. Medusa must never attempt to impersonate the ChatGPT web application or reuse consumer-session credentials. The bridge is a provider-neutral orchestration policy that emits a compact escalation packet and accepts an explicitly supplied answer through a supported integration boundary.

The intended operating model is:

1. Medusa plans, reads, edits, runs tools, verifies, retries, checkpoints, and resumes locally.
2. Existing confidence history and spike gates detect uncertainty, a sharp confidence collapse, missing evidence, or repeated terminally unproductive work.
3. Medusa creates a bounded escalation packet instead of forwarding the full repository or transcript.
4. A configured external reasoning provider, MCP tool, or user-mediated ChatGPT conversation returns advice.
5. Medusa records the advice with provenance, updates confidence, and resumes the same durable session.

## Non-goals

- Circumventing ChatGPT, Codex, API, or provider usage limits.
- Reusing ChatGPT Plus web allowance through undocumented authentication or private endpoints.
- Replacing `medusa-provider` or the normal model loop.
- Sending an entire repository, hidden reasoning, secrets, or unbounded transcripts.
- Allowing escalation advice to execute tools directly.

## Architecture

The bridge should remain a thin layer over capabilities already shipped in Medusa:

- `medusa-confidence` decides whether execution may continue or needs a bounded spike.
- `medusa-failure` distinguishes retryable, terminal, and escalation-worthy failures.
- `medusa-progress` supplies the latest checkpoint and completion evidence.
- `medusa-continuation` resumes the incomplete plan after advice is incorporated.
- `medusa-provider` remains the supported model boundary.
- `medusa-runtime` exposes bridge state consistently to the TUI and desktop clients.

The bridge owns only four concepts:

### Escalation policy

A deterministic policy decides whether to continue locally, run a local spike, request external reasoning, or require the user.

Recommended defaults:

- Continue locally when confidence is at least 65% and no recent collapse exceeds 20 percentage points.
- Run a bounded local spike before escalating when missing evidence can be gathered with read-only tools.
- Escalate after the spike when confidence remains below threshold, the same failure class recurs, or a decision is explicitly marked high-impact and ambiguous.
- Require the user when the decision changes product intent, permissions, irreversible data, credentials, legal obligations, cost commitments, or safety boundaries.
- Cap external escalations per task and apply cooldowns so the bridge cannot become the primary loop accidentally.

### Escalation packet

The packet must be compact, deterministic, redacted, and serializable. It should contain:

- session and todo identifiers;
- current objective and active plan step;
- concise problem statement;
- attempted approaches and observed outcomes;
- relevant evidence references and selected excerpts;
- confidence history summary and gate reasons;
- failure classification;
- explicit decision question;
- bounded answer schema;
- provenance and redaction report.

The packet must not contain provider hidden reasoning, unrestricted file trees, raw secrets, or unrelated session history.

### Advice result

External advice is data, not authority. The result should include:

- recommendation;
- assumptions;
- proposed next actions;
- risks;
- requested evidence;
- confidence estimate;
- source/provenance metadata.

Medusa validates the result, persists it as an immutable session event, and converts accepted actions into normal plan updates. Existing tool policy and approvals remain in force.

### Durable state

A bridge request must survive process restart. Suggested states:

- `not_requested`
- `spike_required`
- `packet_ready`
- `awaiting_external_answer`
- `answer_received`
- `incorporated`
- `rejected`
- `expired`

Each transition should be append-only and include timestamp, policy version, packet digest, provenance, and reason.

## Supported integration modes

### Provider mode

A supported provider configured through `medusa-provider` receives the escalation packet. This uses the provider's documented API and its own usage accounting.

### MCP mode

A configured MCP reasoning tool receives the packet and returns the advice schema. MCP capability discovery, redaction, timeout, and isolation rules apply.

### Manual ChatGPT mode

Medusa renders a copyable escalation packet for the user to paste into ChatGPT web or mobile. The user pastes the answer back into Medusa. This is the only mode that can intentionally use the user's normal ChatGPT conversation allowance without pretending that Medusa itself is ChatGPT.

The manual flow should be ergonomic:

1. `/escalate` or an automatic gate opens the packet preview.
2. The user can copy a Markdown or JSON version.
3. Medusa pauses only the affected todo, not unrelated safe work.
4. The user pastes the answer with `/advice`.
5. Medusa validates, previews the derived plan changes, and resumes.

## Configuration

Suggested configuration surface:

```toml
[escalation]
enabled = true
mode = "manual" # manual | provider | mcp
minimum_confidence_basis_points = 6500
maximum_recent_drop_basis_points = 2000
max_external_requests_per_task = 3
cooldown_seconds = 300
packet_max_bytes = 32768
require_preview = true
```

Environment overrides should use the `MEDUSA_ESCALATION_` prefix. Credentials remain provider-specific and must never be persisted in repository configuration.

## Runtime and UI behavior

The shared runtime should expose:

- why escalation was triggered;
- what local spike was attempted;
- packet size and redaction summary;
- current bridge state;
- request count and cooldown;
- advice provenance;
- the exact plan changes derived from advice.

The TUI and desktop application should use the same runtime commands and events. Suggested commands:

- `/escalate` — force packet preparation for the active todo;
- `/escalate preview` — show the redacted packet;
- `/escalate copy markdown|json` — copy a manual packet;
- `/advice` — paste or attach a returned answer;
- `/escalate reject` — reject pending advice and continue with another strategy;
- `/escalate status` — show counters, cooldown, and state.

## Security and privacy requirements

- Apply existing redaction before serialization and again before transport.
- Default-deny files outside the selected evidence set.
- Include packet byte and item limits.
- Hash the canonical packet and bind the answer to that digest.
- Reject stale answers when the active todo or relevant read-set changed materially.
- Treat advice as untrusted input and never execute embedded commands automatically.
- Preserve all existing approvals, sandboxing, shell policy, network policy, and path safety.
- Record which integration mode and provider produced the answer.

## Acceptance criteria

1. A stable-confidence todo continues without creating an escalation packet.
2. A low-confidence todo first runs a bounded local spike when safe evidence-gathering actions are available.
3. Persistently low confidence creates exactly one redacted packet and enters a durable waiting state.
4. Restarting Medusa preserves the packet, digest, state, counters, and active todo.
5. A valid answer bound to the current packet becomes an immutable event and produces a previewable plan delta.
6. A stale, malformed, oversized, or unbound answer is rejected without changing the plan.
7. Advice cannot bypass tool approvals or execute actions directly.
8. Request caps and cooldowns prevent external reasoning from becoming the normal loop.
9. TUI and desktop expose equivalent bridge behavior through `medusa-runtime`.
10. Documentation clearly states that automatic use of ChatGPT Plus web allowance is unsupported; manual copy/paste is the compliant bridge.

## Delivery sequence

Keep each pull request narrow:

1. Policy, packet schema, canonical hashing, and unit tests.
2. Durable bridge state and session events.
3. Manual packet export and answer import.
4. Runtime commands/events plus TUI and desktop surfaces.
5. Optional provider and MCP adapters.
6. Stale read-set detection, metrics, adversarial tests, and release gates.

This sequence gives Medusa immediate value after step three while keeping provider-specific automation optional.