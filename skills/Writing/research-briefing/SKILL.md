---
name: research-briefing
description: Produce source-backed research briefs, research prompts, or decision-ready summaries. Use for external research, competitor analysis, technical discovery, library/tool evaluation, and evidence-backed writing.
---

# Research Briefing

Use this when facts may be current, niche, disputed, or decision-relevant.

## Research prompt shape

Write a self-contained research question that includes:

- the project/product/context,
- the one decision the research informs,
- 3–6 numbered sub-questions,
- source preferences,
- include/exclude constraints,
- required output format.

## Source hierarchy

Prefer:

1. official docs, changelogs, specs, repositories, release notes,
2. papers, standards, filings, primary datasets,
3. reputable technical analysis,
4. forums/social only as weak signal.

## Research workflow

1. State the question and decision use.
2. Gather sources.
3. Separate confirmed facts, inference, and uncertainty.
4. Compare conflicting sources explicitly.
5. Run a gap pass: what is single-source, stale, or unsupported?
6. Produce a concise synthesis with citations/links where the environment supports them.

## Output format

```markdown
## Research Brief
Question: <question>
Decision it informs: <decision>

## Findings
| Finding | Evidence | Confidence | Why it matters |
|---|---|---|---|

## Recommendation
<decision-ready recommendation>

## Uncertainty / Follow-up
<gaps>
```

## Guardrails

- Do not pad with low-quality sources.
- Do not hide uncertainty.
- Do not treat marketing claims as proof.
- Do not use stale information for current decisions without saying so.
