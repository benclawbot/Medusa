# Agent-created skill drafts

Medusa can turn a high-confidence lesson proposal into a complete skill draft after a successful session.

## Lifecycle

1. The completed session is persisted normally.
2. Automatic lesson extraction creates a verified proposal under `.medusa/learning/proposals/`.
3. Eligible proposals with confidence of at least 700 create a draft under:

   `.medusa/learning/skill-proposals/<skill-name>/`

4. Each draft contains:

   - `SKILL.md`, with the reusable procedure, verification evidence, context, source session, and confidence
   - `manifest.json`, with provenance, proposed installation path, and an explicit approval requirement

## Safety boundary

Generated skills are not written to `.medusa/skills/` and are therefore not loaded by `/skills` or injected into agent prompts. The manifest always records `requires_approval: true`.

Lesson extraction and skill generation are best-effort. A failure in either learning step cannot block or corrupt canonical session persistence.

Credential-like source text is excluded, generated names are normalized to a single safe directory component, content is bounded, and writes use temporary files followed by atomic replacement.

## Scope

This feature creates new skill drafts only. It does not install them, patch existing skills, evaluate competing revisions, expose a review UI, or implement learning metrics. Those remain separate roadmap items and pull requests.
