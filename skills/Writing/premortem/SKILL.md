---
name: premortem
description: "Run a premortem on any plan, launch, product, hire, strategy, or decision. Assumes it already failed 6 months from now and works backward to find every reason why. Produces a revised plan with blind spots exposed. MANDATORY TRIGGERS: 'premortem this', 'premortem my', 'run a premortem', 'what could kill this', 'future-proof this', 'stress test this plan', 'what am i missing here', 'find the blind spots'. STRONG TRIGGERS: 'what could go wrong', 'am i missing anything', 'poke holes in this', 'where will this break', 'devil's advocate this'. Do NOT trigger on simple feedback requests, factual questions, or LLM Council requests. DO trigger when someone has a plan or commitment where the cost of being wrong is high."
---

# Premortem

A premortem is the opposite of a postmortem: imagine the plan already failed, then work backward to explain why before the user commits.

## Context Scan

Before asking the user anything, collect available context for up to 30 seconds:

- Current conversation: extract any plan, launch, product, hire, strategy, decision, audience, constraints, and intended outcome already discussed.
- Workspace: quickly scan likely relevant files such as `memory/`, referenced files, briefs, plans, PRDs, strategy docs, launch notes, or project docs. Prefer targeted glob/search/read calls; do not flood context.
- Attachments or explicit references: use those first.

Then decide whether the minimum context threshold is met:

1. What is it? Be able to describe the thing being premortemed in one sentence.
2. Who is it for or who does it affect? Identify the audience, customers, team, users, or stakeholders.
3. What does success look like? Identify the outcome whose inverse defines failure.

If all three are known or can be reasonably inferred, proceed. If a critical piece is missing, ask the single most important missing question first. Keep it conversational and ask only what is needed:

- "What specifically are you about to launch/build/decide?"
- "Who is this for?"
- "What does a win look like for this?"

## Frame The Premortem

State the frame explicitly before analysis:

> OK, I have enough context. Let's run the premortem. Here's the premise: it is 6 months from now. [The plan/launch/decision] has failed. It's done. We're looking back to understand what went wrong.

This frame is mandatory. It shifts the mode from polite evaluation to honest failure identification.

## Raw Failure Reasons

Generate the raw premortem as one comprehensive analysis:

> This plan has failed 6 months from now. Generate every genuine reason it could have died. Be comprehensive. Be specific. Ground every reason in the actual details of the plan. Do not pad with weak reasons and do not stop early if there are more.

Each failure reason must be:

- Specific to this plan, not generic advice.
- Grounded in details the user provided or files revealed.
- A genuine threat, not a tiny inconvenience or remote edge case.
- Stated in 1-2 sentences.

Use the number of genuine failure reasons the plan deserves. Do not force a fixed count.

## Deep-Dive Agents

Spawn one sub-agent per failure reason, all in parallel. Do not run them sequentially. If a sub-agent facility is unavailable, run independent deep dives in separate isolated passes and explicitly note the limitation in the transcript.

Use this prompt template for each agent:

```text
You are an investigator in a premortem analysis. You've been assigned one specific failure reason to analyze in depth.

The plan:
---
[full context: what it is, who it is for, what success looks like, plus relevant workspace context]
---

PREMORTEM FRAME: It is 6 months from now. This plan has failed.

YOUR ASSIGNED FAILURE REASON: [the specific failure reason from the raw premortem]

Your job is to go deep on this one failure. Write the story of how it actually played out. Be specific. Use details from the plan. Make it feel real, like a case study of something that actually happened.

Your output should include:

1. THE FAILURE STORY: A 2-3 paragraph narrative of how this specific failure played out. Use details from the plan. Name specific moments where things went wrong and why.
2. THE UNDERLYING ASSUMPTION: The one thing the user was taking for granted that made this failure possible. State it in one sentence.
3. EARLY WARNING SIGNS: 1-2 concrete, observable signals the user could watch for that would indicate this failure mode is starting to play out. These should be things the user can actually see or measure, not vague feelings.

Keep the total response under 300 words. Be direct. Do not hedge. Do not sugarcoat.
```

## Synthesis

Read every deep dive and produce a `PREMORTEM REPORT` with:

1. The Most Likely Failure: the failure scenario most probable given the plan, and why.
2. The Most Dangerous Failure: the scenario that would cause the most damage even if less likely.
3. The Hidden Assumption: the single biggest unexamined assumption across the analyses.
4. The Revised Plan: concrete changes that make the plan more resilient. Each change must map to a specific failure scenario and be actionable this week.
5. The Pre-Launch Checklist: 3-5 specific things to verify, test, or put in place before executing.

Make the synthesis specific and blunt. The revised plan must use concrete actions, not vague advice.

## Output Files

Every premortem session must produce two files in the user's workspace:

```text
premortem-report-[timestamp].html
premortem-transcript-[timestamp].md
```

Use a filesystem-safe timestamp such as `YYYYMMDD-HHMMSS`.

### HTML report

Create one self-contained HTML file with inline CSS, then open it.

Design requirements:

- Dark background, such as `#0a0e1a`, clean typography, and easy scanning.
- Put the synthesis prominently at the top.
- Show one visual card per failure reason with the failure reason, failure story, underlying assumption, and warning signs.
- Use distinct accent colors per card.
- Include a clear severity/likelihood indicator for each failure mode.
- Include a grid showing the number of agents/deep dives and their findings.
- Footer with timestamp and what was premortemed.

### Transcript

Create a Markdown transcript containing:

- Gathered context: what, who, success criteria, and relevant workspace context.
- Raw failure reasons.
- All deep dives.
- Full synthesis.
- Any limitations, such as sub-agent tools being unavailable.

## Chat Summary

After creating the files and opening the HTML report, respond in chat with no more than three sentences:

- Most likely failure.
- Hidden assumption.
- Single most important revision.

Include the paths to both files. The full details belong in the report and transcript, not the chat.

## Boundaries

- Do not trigger on simple feedback requests, factual questions, or LLM Council requests.
- If the user wants multiple perspectives on a decision right now rather than failure analysis from the future, suggest the council instead.
- Do not sugarcoat serious problems.
- Do not run a premortem on insufficient context; ask one focused question instead.
