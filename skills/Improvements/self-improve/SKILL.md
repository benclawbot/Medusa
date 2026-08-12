---
name: self-improve
description: Errors-driven retrospective that auto-applies low-risk guardrail fixes and presents the rest for approval. Use when asked to "self-improve", "reflect on session", "what can we improve", "session retrospective", "end of session review". Analyzes session errors/warnings, clusters them, judges preventability, applies or queues fixes (skills, AGENTS.md, memory, hooks, scripts), and logs everything to a git-tracked ledger.
disable-model-invocation: true
---

# Self-Improve

Two phases. Phase A (errors-driven, automated) runs first. Phase B (general reflection, ask-first) is the existing flow. Outputs are unified into one queue and presented at the end.

## Hard limits

- **Cooldown**: refuse to run twice in the same session unless the user explicitly invokes `/self-improve` again (e.g., `again`, `force`). Check via `ctx_search(queries: ["self-improve", "ran self-improve"], sort: "timeline")` for entries from the current session.
- **Auto-apply cap**: maximum **3** Tier A fixes per invocation.
- **Queue cap**: maximum **5** Tier B fixes per invocation.
- **Tier C (Pi core source)**: always ask, never auto-apply.
- **No fixes outside the harness**: project files (code, configs in the user's repo) are out of scope for auto-apply unless the user is currently working in that project and the fix is local (e.g., adding a missing `.gitignore` rule the project needs).
- **Invocation contract**: when this skill content appears in your context via slash command (`/self-improve` or any short form the user typed that resolved to this skill), the user has EXPLICITLY invoked it. Execute all phases immediately. Do not ask "do you want me to run this?" — the slash command IS the request. `disable-model-invocation: true` means "don't auto-load on context match", not "ignore slash command invocations".

---

## Phase A — Errors-Driven (automated)

### A1. Gather errors and warnings

Pull from all available sources, in order of signal strength:

1. **Auto-captured session events** via `ctx_search`:
   ```
   ctx_search(queries: ["error", "tool failure", "blocker", "rejected approach"], sort: "timeline", limit: 20)
   ```
2. **Manual `/btw` messages** from the current session: read with `btw_read`.
3. **Visible conversation**: scan the current turn history for error output, warnings, retries, or "the agent said X but should have said Y" patterns.
4. **Cross-session aggregation**: re-run the queries with `source: "error"` and `source: "error-resolution"` across the persistent index to find recurring patterns. A single occurrence = noise; ≥2 occurrences across distinct sessions = signal.

Skip pure environment noise (network timeouts, missing optional deps, OS permission denials the user can't fix).

### A2. Cluster by error class

Group errors that share the same root cause. Examples of clusters:
- "git init ran in wrong directory" (parent-dir confusion)
- "tool called with wrong argument shape"
- "skill not loaded because trigger phrase didn't match"
- "agent guessed instead of asking when input was ambiguous"

One cluster = one fix candidate. Do not produce 5 patches for 5 instances of the same bug.

### A3. Judge preventability

For each cluster, answer in one sentence:
- **Could a guardrail have prevented this?** (yes/no)
- **Where?** (skill text, AGENTS.md rule, memory fact, pre-tool-use hook, new skill, script)
- **Confidence**: 0.0–1.0 (how sure are you the proposed fix would have caught it?)

Discard clusters with `confidence < 0.6` or `preventable = no`. List them in the "Skipped" section so the user sees what was considered.

### A4. Skill trigger audit (bonus pass)

While reading existing skills, check:
- Does each skill's `description:` field actually match what it does?
- Are there trigger phrases in the description that no skill currently handles?
- Are there skills that have been used this session but weren't loaded automatically because the trigger missed?

Fix candidates from this pass are tagged `skill-audit` and treated as Tier A if confidence ≥0.9.

### A5. Contradiction check

Before adding any rule, search for existing rules that conflict:

```bash
# For skill text additions
grep -r "<key phrase from new rule>" "C:/Users/thoma/.pi/agent/" "C:/Users/thoma/.agents/"
# For project rules
grep -r "<key phrase>" .claude/ CLAUDE.md AGENTS.md 2>/dev/null
```

If a conflict is found: drop the new rule and surface it in "Skipped" with the conflicting text quoted. The user can resolve manually.

### A6. Generalization filter

Apply these rules before promoting a cluster to a fix candidate:

| Condition | Outcome |
|---|---|
| Cluster seen ≥2 distinct contexts (different sessions, different projects) | Promote to fix candidate |
| Cluster seen once, but is a clear safety issue (data loss, security, irreversible op) | Promote to fix candidate |
| Cluster seen once, not safety-related | Skip — too noisy, mention in "Skipped" |
| Cluster is a one-off user preference | Skip — goes to memory fact instead, ask user |

### A7. Tier classification

For each surviving fix candidate:

| Tier | Conditions | Behavior |
|---|---|---|
| **A — auto-apply** | Target is `~/.pi/agent/skills/*.md`, `~/.pi/agent/AGENTS.md`, or `~/.pi/agent/memory*.md` AND confidence ≥0.9 AND no contradiction | Apply silently, log to ledger |
| **B — queue for approval** | Target is a new skill, a hook, a script in `~/.pi/agent/scripts/`, or a project config | Add to queue with diff preview |
| **C — always ask** | Target is **Pi core source** (under `node_modules/@earendil-works/pi-coding-agent/` or wherever the global `pi` install lives) OR target is a system prompt template OR target is the pi binary itself | Add to queue, but flag with `⚠ PI CORE` and explain that this requires either forking Pi or modifying the installed package (which won't survive reinstall) |

To detect Pi core: if the proposed edit path contains `@earendil-works/` or matches `node_modules/pi*` or `*/pi-coding-agent/`, it's Tier C.

### A8. Apply Tier A

For each Tier A fix:

1. Compute the diff (use `git diff <file>` to capture current state).
2. Apply the edit.
3. Add a row to the ledger (see A10). The ledger entry includes a one-line "Verify" recipe inline — no separate file.

Cap at 3 Tier A fixes per invocation. If more survive the filters, push the rest to Tier B for approval.

### A9. Present Tier B + Tier C

For each queued fix, print:

```
## Proposed Fixes (awaiting approval)

### [B1] Edit: ~/.pi/agent/skills/<name>/SKILL.md
**Cluster**: <error class> (N occurrences)
**Change**:
```diff
- old line
+ new line
```
**Reason**: <why this would have prevented the error>

### [C1] ⚠ PI CORE: Edit: <path>
...
```

Then ask:

> Approve which? (all B / all C / numbers / none / show-diff <n>)

Do not apply anything until the user responds. If user says "all B but skip C", apply B's and skip C's with explanation.

---

## Phase B — General Reflection (existing flow)

Run the existing 10-area reflection. Add findings to the same unified queue, classified by tier using the same rules.

| Area | What to Look For |
|------|------------------|
| **Agent config** | Could AGENTS.md instructions be clearer? Did the agent misunderstand something better wording would prevent? |
| **Subagent behavior** | Did subagents struggle, go off-scope, or need repeated correction? Would better task descriptions help? |
| **Agent definitions** | Check `~/.pi/agent/agents/*.md` — are model choices, skills, or system prompts optimal? |
| **Tests** | Were bugs found that tests should catch? Are existing tests stale? |
| **Documentation** | Are READMEs, inline docs, or references out of date after changes this session? |
| **Scripts** | Did any scripts fail, produce wrong output, or need workarounds? |
| **Extensions & MCP** | Were MCP servers or extensions used that could be better configured? Were tools missing that would have helped? |
| **Skills** | Did any skill produce suboptimal results? Are trigger descriptions accurate? |
| **Code quality** | Did the session reveal patterns worth refactoring, error handling gaps, or repeated boilerplate? |
| **Workflow** | Were there unnecessary back-and-forth cycles, wasted API calls, or inefficient tool usage patterns? |

Findings from Phase B default to Tier B (ask first). They only go Tier A if all conditions are met (skill/AGENTS.md/memory target, confidence ≥0.9, no contradiction).

---

## Ledger (`~/.pi/agent/improve-log.md`)

Append every auto-applied fix. Format:

```markdown
## <ISO timestamp> — session <id>

**Tier**: A
**Cluster**: <error class>
**Confidence**: 0.92
**File**: ~/.pi/agent/skills/foo/SKILL.md
**Diff**:
```diff
- old
+ new
```
**Verify**: <one-line: how to confirm the rule fires>
**Reason**: <one-line>
```

The ledger file lives at `~/.pi/agent/improve-log.md` (this repo). Every entry is committed:

```bash
cd ~/.pi/agent
git add skills/ AGENTS.md memory*.md improve-log.md
git commit -m "self-improve: <one-line summary of auto-applied fix>"
```

If `~/.pi/agent/` is not a git repo (check first with `git rev-parse --show-toplevel`), skip the commit and warn the user.

---

## Output Summary

After Phase A and Phase B, print one summary:

```
## Self-Improve Report

### Auto-applied (Tier A)
- [A1] Fixed "git init parent-dir confusion" in github-commit-push skill — confidence 0.95

### Awaiting approval (Tier B)
- [B1] Add pre-tool-use hook blocking `git add .` outside project root — cluster: same data-loss class
- [B2] Update spec-driven-development trigger description (mentions "spec" but misses "design doc")

### Awaiting approval (Tier C — Pi core)
- [C1] ⚠ PI CORE: Patch tool-call JSON parser to surface field-level errors — requires fork

### Skipped (confidence < 0.6 or non-actionable)
- Cluster "network timeout fetching X" — environment noise, no guardrail helps
- One-off user typo — memory fact, will ask

### Ledger
~/.pi/agent/improve-log.md updated, committed as <sha>
```

---

## Edge cases

- **No errors found**: skip Phase A, run Phase B only.
- **All clusters fail filters**: print "No actionable clusters found. Running Phase B." and continue.
- **Cooldown triggered**: print "self-improve already ran this session. Run `/self-improve again` to force." and stop.
- **`gh` not authenticated, can't push ledger commit**: warn, write ledger entry uncommitted, continue.
- **User invokes with `--dry-run`**: do all analysis, print all proposed diffs, apply nothing, do not write ledger.
- **Tier C path detection false positive**: if the user explicitly says "this is fine to modify", override and treat as Tier B with a note.

---

## When NOT to use this skill

- Pre-commit fix (typo, formatting) → use `commit` skill
- One-off debugging → standard tools
- Mid-task mid-flight changes → finish the current task first
