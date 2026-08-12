---
name: skill-curation
description: Create, review, merge, split, or improve agent skills. Use for SKILL.md authoring, new skill scaffolding, skill routing conflicts, duplicate skills, frontmatter validation, generic skill packs, and skill maintenance.
---

# Skill Curation

A good skill is a focused workflow, not a knowledge dump.

## Skill anatomy

```markdown
---
name: lowercase-hyphen-name
description: What it does, when to use it, and how it differs from related skills.
---

# Skill Name

## When to use
## Workflow
## Output format
## Failure modes
```

## Routing contract

The description is for selection. It should say:

- what the skill does,
- when to use it,
- what makes it different from similar skills.

Do not put the full workflow in the description.

## Merge/split rules

Merge skills when they:

- trigger on the same situations,
- solve the same problem at different depth,
- force the agent to choose between near-duplicates,
- create conflicting instructions.

Split skills when they:

- have different users or outputs,
- combine unrelated concerns,
- mix deterministic tool usage with judgment-heavy reasoning,
- are so long that the agent will miss critical instructions.

## Create/update workflow

When creating or changing a skill:

1. Clarify the skill's single job, expected triggers, required tools, and target folder.
2. Choose a lowercase hyphenated `name` that is unique across the whole skills tree.
3. Write frontmatter first; keep `description` focused on routing, not the full procedure.
4. Add references, scripts, or assets only when they remove repeated work.
5. Validate by checking YAML frontmatter, duplicate names, broad trigger overlap, and whether a likely user request selects the intended skill.

## Generic-default rules

For a default Zeus skill pack:

- avoid personal names,
- avoid single-agent product assumptions,
- avoid local machine paths,
- avoid model-specific claims,
- keep vendor-specific commands in optional references, not core instructions.

## Validation checklist

- folder name matches `name`, unless the host explicitly supports aliases,
- YAML parses,
- description is actionable,
- workflow is testable,
- failure modes are documented,
- no secrets or private paths,
- no hidden instructions unrelated to the skill.
