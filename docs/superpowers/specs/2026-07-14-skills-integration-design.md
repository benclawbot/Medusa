# Skills Integration Design

## Goal

Give Medusa a request-driven skill auto-trigger, mirroring the way Claude Code surfaces
its bundled `superpowers` skills. The user types a request, the engine matches it against
the `triggers` of every available skill, reranks the candidates with a small model call,
loads the chosen skill (and any skills it `requires`, in declaration order), and injects
their bodies into the working context before the model responds. A skill can also
declare a `handoff` that triggers a re-match after the current turn completes.

The starter set is a mirror of the 14 skills in the bundled superpowers plugin:
brainstorming, dispatching-parallel-agents, executing-plans, finishing-a-development-branch,
receiving-code-review, requesting-code-review, subagent-driven-development,
systematic-debugging, test-driven-development, using-git-worktrees, using-superpowers,
verification-before-completion, writing-plans, writing-skills.

The canonical source root for this design is
`Documents/Codex/2026-07-13/upd/work/medusa` on branch `main` at
`64da59db1897a1e4b0b17e5d6c84e4f530e03b69` (in sync with `origin/main`).
The implementation lands on a new worktree at
`Documents/Codex/2026-07-13/upd/work/medusa-skills-integration` on branch
`medusa/skills-integration` branched from `main`.

## Scope

In scope:

- A `medusa-skills` crate that ships the 14 mirrored skills as a single
  embedded-asset directory inside the `medusa` binary, plus a `manifest.json`
  index that lists each skill's name, version, triggers, tools, permissions,
  and `requires` / `handoff` declarations.
- A new `medusa-agent/src/skill_matcher.rs` module that, given a user prompt
  and the manifest index, returns the ranked list of matching skills. The
  matcher is two-stage: a fast keyword pre-filter, then a single LLM rerank
  call when more than one skill matches.
- A new `medusa-agent/src/skill_loader.rs` module that resolves the chosen
  skill (and any `requires` chain, recursively) into a single `SkillBundle`
  with bodies in load order.
- A new `medusa-agent/src/skill_injector.rs` module that produces the prompt
  section injected before the model responds. The section names each loaded
  skill, lists its `triggers`, and quotes the body verbatim.
- A new `skill_handoff` field on `AgentSession` that, if a loaded skill
  declares a `handoff`, queues a re-trigger of the matcher after the current
  turn completes. The next user turn re-evaluates the matcher.
- A new `SkillChain` test fixture crate that gives us a deterministic way to
  test chain loading and handoff without an LLM.
- Per-skill `SKILL.md` files under `medusa-skills/skills/<name>/SKILL.md`,
  each with a YAML frontmatter matching the `SkillManifest` already defined
  in `crates/medusa-extensions/src/skills.rs`. 14 directories total.
- Configuration: a `SkillConfig` struct in `medusa-config` with `enabled`
  (default true), `bundle_path` (default = embedded), `max_matches` (default
  5), `max_chain_depth` (default 4), and `matcher_mode` (`Keyword | KeywordLlmRerank`,
  default `KeywordLlmRerank`).
- Telemetry: a counter on `AgentSession` that records how many times each
  skill was matched, injected, and handed off. Surfaced through the existing
  `medusa-hardening` observability surface.

Out of scope (separate sub-projects):

- Anything from the previously paused `medusa-extended-reach-and-no-truncation`
  plan. The two sub-projects merge independently.
- Domain-specific skills (predicting markets, building a portfolio, etc.) —
  those are third-party skills, not part of the mirrored set.
- Cross-project skill sharing. Skills ship embedded with the binary; an
  installable-skill format is a future sub-project.

## Architecture

```
┌────────────────────────────┐         ┌────────────────────────────────┐
│ medusa-tui (TUI)           │         │ medusa-agent                   │
│  ┌──────────────────────┐  │  ipc    │  ┌──────────────────────┐      │
│  │ AppState / renderer  │◀─┼─────────┼─▶│ engine.rs            │      │
│  └──────────────────────┘  │        │  │  • tool router       │      │
└────────────────────────────┘        │  │  • per-turn pipeline │      │
                                      │  │     1. user prompt   │      │
                                      │  │     2. skill_matcher │      │
                                      │  │     3. skill_loader  │      │
                                      │  │     4. skill_injector│      │
                                      │  │     5. model call    │      │
                                      │  │     6. handoff check │      │
                                      │  └──────────────────────┘      │
                                      │  ┌──────────────────────┐      │
                                      │  │ skill_matcher.rs     │      │
                                      │  │  • keyword filter    │      │
                                      │  │  • LLM rerank        │      │
                                      │  └──────────────────────┘      │
                                      │  ┌──────────────────────┐      │
                                      │  │ skill_loader.rs      │      │
                                      │  │  • resolve requires  │      │
                                      │  │  • depth cap         │      │
                                      │  │  • cycle detection   │      │
                                      │  └──────────────────────┘      │
                                      │  ┌──────────────────────┐      │
                                      │  │ skill_injector.rs    │      │
                                      │  │  • format bodies     │      │
                                      │  │  • cite triggers     │      │
                                      │  └──────────────────────┘      │
                                      └────────────┬───────────────────┘
                                                   │ embedded
                                                   ▼
                                      ┌────────────────────────────┐
                                      │ medusa-skills (lib)         │
                                      │  • skills/<name>/SKILL.md  │
                                      │  • manifest.json (index)    │
                                      │  • bundled at build time    │
                                      └────────────────────────────┘
```

### New crates

- `crates/medusa-skills/Cargo.toml` — library crate.
- `crates/medusa-skills/src/lib.rs` — re-exports the manifest index and the
  asset reader. Knows nothing about the agent; pure data.
- `crates/medusa-skills/assets/manifest.json` — the index, generated from
  the per-skill YAML frontmatter at build time by a `build.rs` script.
- `crates/medusa-skills/assets/skills/<name>/SKILL.md` — one per mirrored
  skill. 14 directories total.

### Changed crates

- `crates/medusa-agent/Cargo.toml` — add `medusa-skills` dep.
- `crates/medusa-agent/src/lib.rs` — wire the new modules.
- `crates/medusa-agent/src/engine.rs` — per-turn pipeline inserts the
  skill-matcher step between the user prompt and the model call; checks
  the handoff queue at the end of the turn.
- `crates/medusa-agent/src/session.rs` — add `skill_handoff: Vec<SkillRef>`
  on `AgentSession`. Cleared on session resume.
- `crates/medusa-config/src/lib.rs` — `SkillConfig` struct, env var readers.
- `crates/medusa-hardening/src/observability.rs` — extend the counter
  surface to include the new skill-match events.

### New modules in medusa-agent

- `crates/medusa-agent/src/skill_matcher.rs`
  - `pub struct SkillMatch { pub skill: SkillRef, pub score: f32, pub matched_triggers: Vec<String> }`
  - `pub fn match_prompt(prompt: &str, index: &SkillIndex, config: &SkillConfig) -> MedusaResult<Vec<SkillMatch>>`
  - `pub enum MatcherMode { Keyword, KeywordLlmRerank }`
  - The keyword pre-filter: case-insensitive substring search across the
    user prompt; scores by `matched_triggers.len()`; filters to `config.max_matches`.
  - The LLM rerank: when `config.max_matches > 1` and `matcher_mode ==
    KeywordLlmRerank`, send a tiny prompt to the configured provider
    ("Given a user request and a list of candidate skills with their
    descriptions, return the IDs of the best-matching skills, ranked most-
    relevant first. Return at most `max_matches` ids. JSON only."). The
    response is parsed and used to reorder `SkillMatch` results.
- `crates/medusa-agent/src/skill_loader.rs`
  - `pub struct SkillBundle { pub entries: Vec<SkillEntry> }`
  - `pub struct SkillEntry { pub skill: SkillRef, pub body: String, pub depth: usize }`
  - `pub fn load(index: &SkillIndex, root: SkillRef, config: &SkillConfig) -> MedusaResult<SkillBundle>`
  - Recursively resolves `requires:` declarations. Detects cycles (a → b → a).
  - Caps depth at `config.max_chain_depth`. Rejects cycles with a
    `PolicyDenied` error.
- `crates/medusa-agent/src/skill_injector.rs`
  - `pub fn render(bundle: &SkillBundle) -> String`
  - Produces a section like:
    ```
    The following skills were loaded for this turn (matched by trigger):
    - [brainstorming] triggers: [brainstorm, design, idea, ...]
      <body>
    - [writing-plans] (required by brainstorming)
      <body>
    ```
  - The section is prepended to the system prompt, not the user prompt.

### Build-time codegen

A `build.rs` script in `medusa-skills` walks the 14 `SKILL.md` files at
build time, parses their YAML frontmatter, validates the manifest against
the same rules as `medusa-extensions::skills::validate_skill_manifest`,
and emits `assets/manifest.json`. The crate then embeds the whole `assets/`
directory via `include_dir!` (already used elsewhere in the workspace) or
the `embedded-fs` crate if `include_dir!` is not in the dep tree.

## Data flow

One user turn, end to end:

```
user types prompt
  └─ TUI sends to engine via ipc
        └─ engine::on_user_turn(prompt, session)
              │
              ├─► skill_matcher::match_prompt(prompt, index, config)
              │     │
              │     │  keyword pre-filter → [m1, m2, m3]  (top N)
              │     │  LLM rerank          → [m1, m3]     (reranked)
              │     │
              │     └─► top match: skill A
              │
              ├─► skill_loader::load(index, A, config)
              │     │
              │     │  A requires B
              │     │  B requires C
              │     │  → bundle = [A, B, C]
              │     │
              │     └─► SkillBundle { entries: [A, B, C] }
              │
              ├─► skill_injector::render(&bundle)
              │     └─► "## Loaded skills\n- [A] ...\n- [B] ...\n- [C] ..."
              │
              ├─► session.system_prompt = base + injected
              │
              ├─► provider call with system + user + history
              │     └─► model returns assistant message + tool calls
              │
              ├─► engine executes tool calls
              │     └─► (full body, output_envelope, etc.)
              │
              ├─► if loaded skill declared handoff:
              │     └─► session.skill_handoff.push(skill_handoff_target)
              │
              └─► end of turn
```

Next user turn:

```
user types prompt
  └─ engine::on_user_turn(prompt, session)
        │
        ├─► if session.skill_handoff is non-empty:
        │     └─► force-match against the handoff target
        │           (no LLM rerank, deterministic)
        │
        └─► then proceed with normal match_prompt
```

## Error handling

- A skill's `SKILL.md` has a malformed manifest at build time → build fails.
  This is a release-blocking error.
- A `requires:` cycle → `PolicyDenied` error at the start of the turn. The
  engine surfaces the cycle to the user via `error:` and skips the bundle.
- `max_chain_depth` exceeded → `PolicyDenied` error. Same path.
- The LLM rerank returns invalid JSON or unexpected IDs → fall back to the
  keyword-only ordering. The matcher logs a warning via
  `medusa-hardening::observability`.
- The bundle is too large to fit in the system prompt (after counting the
  base prompt and the user prompt) → the injector truncates each skill body
  to a per-skill byte budget, with a clear `[truncated at N bytes — full
  body at <path>]` marker. The full body is written to
  `<session>/artifacts/skill_<name>_<ulid>.md` using the same envelope
  helper from the browser/display plan (assuming that sub-project merges
  first; if not, a tiny new write helper).
- The user invokes a slash command like `/skill writing-plans` → the slash
  command path bypasses the matcher and forces `writing-plans` into the
  bundle. The slash-command path uses the same `skill_loader` and
  `skill_injector` modules, so the bundle shape is consistent.

## Testing

Unit (in `medusa-agent` and `medusa-skills`):

- `skill_matcher::match_prompt` for: empty prompt, prompt with no triggers,
  prompt matching one skill, prompt matching N skills, LLM rerank stub that
  returns the same order, LLM rerank stub that reverses, LLM rerank that
  returns invalid JSON (fall back path).
- `skill_loader::load` for: single skill, chain of two, chain of three,
  cycle detection (a→b→a), depth cap, missing `requires` target, manifest
  validation failure on the chain target.
- `skill_injector::render` for: empty bundle, single-skill bundle, chain
  bundle, bundle with triggers present, truncated bundle.
- `medusa-skills` asset loading: 14 skills present, manifest matches the
  on-disk files, all manifests validate.

Integration (in `medusa-agent`, gated on a mock provider):

- End-to-end "user types a request → model receives the injected skill
  body" with a fake provider that records the system prompt it was given.
  Asserts the system prompt contains the matched skill's body and trigger
  list, and that the user prompt is unchanged.
- Handoff test: skill A declares `handoff: B`. After the turn, the next
  turn forces B into the bundle without rerunning the matcher.

Build-time:

- `medusa-skills/build.rs` tests: valid manifest builds, invalid manifest
  fails with a clear error, missing `SKILL.md` fails, duplicate skill
  names fail.

End-to-end (gated on a real provider):

- `medusa --repo /tmp/test-repo --prompt "help me design a new feature"`
  in a fixture repo with a `brainstorming` skill installed. Assert the TUI
  shows the brainstorming skill's body in the system-prompt section and
  the model response references the skill.

## Validation

- Spec self-review (this section) — no TBDs, no contradictions, scope is
  one plan, no ambiguity in the matcher / loader / injector / handoff
  semantics.
- User review of the written spec at
  `docs/superpowers/specs/2026-07-14-skills-integration-design.md`.
- `superpowers:writing-plans` produces a step-by-step implementation plan
  that is reviewed in turn before any code is written.

## Alternatives Rejected

- **Embedding the LLM rerank as a separate always-on tool call.** That would
  add a round-trip per turn. The keyword pre-filter is enough to bring the
  candidate set to `max_matches` (default 5), so the rerank only fires when
  the candidate set is non-trivial.
- **Loading all skills into the system prompt unconditionally.** Wastes
  tokens, dilutes the model's attention, and makes the agent feel
  cookie-cutter. The whole point of a trigger is selectivity.
- **Using embeddings for the matcher.** Non-deterministic, costs more,
  harder to test, and the superpowers trigger strings are already designed
  as discriminative keywords. Embeddings would buy little.
- **Implementing chaining as a graph traversal in the engine itself, with
  a workflow DSL.** Massive scope creep. The `requires` + `handoff`
  pattern is the smallest design that captures the actual use case
  (brainstorm → plan, plan → execute, etc.) without inventing a new
  language.
- **Mirroring only a subset of the superpowers skills.** Inconsistent:
  the user would expect the same set they see in Claude Code, and partial
  mirroring is worse than no mirroring because it changes the user's mental
  model. The cost of mirroring all 14 is bounded — each skill is a
  Markdown file, not new code.

## Open Questions

None. All clarifying questions have been answered:

- Scope: engine + skills + multi-skill chaining.
- Trigger mechanism: keyword pre-filter + LLM rerank.
- Trigger timing: once per user turn.
- Starter set: mirror the full superpowers set.
- Chaining model: reference + auto-load + handoff.
