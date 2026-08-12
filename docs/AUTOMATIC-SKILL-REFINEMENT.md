# Automatic Skill Refinement

Medusa can refine an existing inactive skill draft when a later verified lesson maps to the same normalized skill name.

## Scope

Refinement applies only to drafts under:

```text
.medusa/learning/skill-proposals/<skill-name>/
```

It never edits active project or user skills under `.medusa/skills` or the user skill directory.

## Refinement rules

A later lesson may refine a draft only when:

- the lesson is still in `proposed` state;
- confidence is at least 700/1000;
- the repository fingerprint matches the existing draft;
- the lesson has not already been applied;
- procedure and verification evidence are present;
- secret-like content is rejected.

Medusa then:

1. archives the current `SKILL.md` under `revisions/<revision>.md`;
2. merges new procedure and verification items without duplicates;
3. merges observed tools;
4. keeps each section bounded;
5. records every contributing lesson and session in `manifest.json`;
6. increases confidence only to the maximum verified confidence observed;
7. increments the draft revision;
8. keeps `requires_approval` set to `true`.

## Idempotency

Reprocessing the same lesson ID does not create another revision or duplicate content.

## Safety boundary

Automatic refinement improves a reviewable proposal only. Installation, activation, rollback policy, review UI, and provenance scoring remain separate roadmap items.
