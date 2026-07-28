# Medusa Usability Study Kit

Issue: #421

This directory contains the versioned artifacts required to prepare, run, score, and report Medusa's task-based usability study for first-run, execution-state comprehension, approvals, failed verification, recovery, rollback preview, and audit export.

## Artifact order

Use the files in this order:

1. [`protocol-v1.md`](protocol-v1.md) — pre-registered goals, metrics, success thresholds, privacy rules, and facilitator constraints.
2. [`recruitment-and-sampling-v1.md`](recruitment-and-sampling-v1.md) — cohort definitions, screening, assignment matrix, exclusions, and reserve policy.
3. [`fixture-spec-v1.md`](fixture-spec-v1.md) — deterministic study scenarios, reset contract, self-check, and platform validation matrix.
4. [`participant-script-v1.md`](participant-script-v1.md) — verbatim participant prompts and task-specific follow-up questions.
5. [`session-run-checklist-v1.md`](session-run-checklist-v1.md) — per-session validity, consent, evidence, assistance, safety-stop, and sanitization checks.
6. [`findings-template-v1.md`](findings-template-v1.md) — aggregate scoring, severity/frequency reporting, follow-up issue tracking, and final pass/fail conclusion.

## Study lifecycle

### 1. Freeze the study version

Before recruitment:

- choose one protocol version
- choose one fixture version
- pin the Medusa commit and client builds
- complete the fixture platform/client matrix
- verify the pre-registered thresholds have not been modified after pilot observations

### 2. Recruit and assign

Use the recruitment plan to fill at least six valid participant slots plus reserves. Ensure at least two true first-time users, at least two experienced users, at least two operating systems, and supported client coverage are represented.

Store names, contact details, scheduling information, and compensation data outside the repository.

### 3. Validate the environment

Before every scored session:

- reset the disposable fixture
- run the fixture self-check
- verify the planned client/platform/scenario combination
- confirm raw observation storage is outside the repository
- confirm no production credential or private repository is present

A failed pre-session validity gate means the scored session must not begin.

### 4. Run the session

Read the participant script verbatim. Record task outcomes, elapsed time, wrong turns, assistance, confidence, state comprehension, and blocking-reason comprehension using the session checklist.

Use neutral facilitator prompts. Intervene immediately for credential exposure, non-fixture mutation, or an out-of-scope approval risk.

### 5. Sanitize evidence

Before evidence is shared or committed:

- replace participant identity with a study code
- remove usernames, machine names, home paths, emails, account IDs, credentials, and private source content
- omit raw recordings unless separately consented and securely handled
- retain only the minimum evidence needed to reproduce navigation and state-transition problems

Raw participant data must not be committed.

### 6. Aggregate and report

Use the findings template after the study wave is complete. Report exclusions and protocol deviations, not only valid completions. Do not change thresholds after results are known.

Every critical or high-severity usability failure requires a concrete follow-up issue with sanitized reproduction evidence and acceptance criteria.

## Validity checklist

A study conclusion is valid only when all are true:

- at least six participant sessions are valid
- at least two participants are true first-time coding-agent users with no prior substantive coding-agent task
- at least two experienced coding-agent users are represented
- at least two operating systems are represented
- success thresholds were fixed before scored sessions
- quickstart, timeline/state comprehension, approvals, failed verification, interruption/resume, rollback preview, and audit export were exercised
- no participant identity, credential, or private repository content appears in committed artifacts
- every critical or high-severity finding has a tracked issue

If any condition is unmet, report the study as incomplete or unable to evaluate rather than passing it.

## Versioning

Material changes to prompts, fixture behavior, expected results, scoring, thresholds, cohort definitions, or assistance rules require a new version. Identify exactly which participants used each version; do not aggregate materially different versions without explaining the limitation.

Typographical fixes that do not affect participant behavior or scoring may remain within the same version, but should still be recorded in study change notes.

## Repository boundary

This directory contains process and sanitized aggregate artifacts only. It must never contain:

- participant names or contact information
- payment details
- provider credentials
- private repository content
- unsanitized terminal history
- raw audio, video, screenshots, or transcripts
- live-provider request or response bodies containing user data