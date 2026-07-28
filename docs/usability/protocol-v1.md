# Medusa Task-Based Usability Protocol v1

Status: pre-registered study protocol  
Issue: #421  
Version: 1.0  

## Purpose

Evaluate whether people who did not build Medusa can complete core workflows, understand execution state, respond safely to approvals, diagnose failure, recover interrupted work, and interpret the audit trail without repository-specific assistance.

This protocol defines success criteria before participant sessions begin. Do not tune thresholds or scoring after observing results; record any protocol deviations explicitly in the findings report.

## Study sample

Recruit at least six participants and satisfy all of the following:

- at least two first-time coding-agent users
- at least two experienced coding-agent users
- at least two operating systems represented
- Linux, macOS, and Windows represented where practical
- at least one participant unfamiliar with Rust tooling

Record only cohort, operating system, prior coding-agent experience, and relevant accessibility needs. Do not commit names, contact details, repository content, provider credentials, or screen recordings.

## Test environment

Use a clean supported installation or launch path and a disposable repository prepared for the study. The repository must contain:

- a bounded quickstart task with an objectively verifiable result
- a realistic multi-file task
- one scripted sensitive operation that requires approval
- one deterministic failed verification step
- one resumable interrupted session
- one checkpoint suitable for rollback preview
- audit export enabled

Pin and record:

- Medusa commit SHA and build provenance
- operating system and version
- client used: TUI or desktop
- provider/model configuration category, excluding secrets
- fixture repository commit SHA
- protocol version

## Privacy and consent

Before starting, explain what will be observed and obtain explicit consent.

Allowed observations:

- task completion and abandonment
- elapsed time per task
- navigation and state-transition events
- wrong turns and assistance requested
- confidence ratings
- participant explanations of current state

Prohibited collection:

- provider credentials or API keys
- source code or repository contents not created for the study
- personal identifiers in committed artifacts
- raw terminal history containing secrets
- audio, video, or screenshots without separate explicit consent

Diagnostic capture must be opt-in, local, minimal, and reviewed for secrets before export. Delete raw participant data according to the study consent statement.

## Facilitator rules

- Read task prompts exactly as written in the participant script.
- Do not teach product concepts before the participant encounters them.
- Use neutral prompts such as “What are you looking for?” and “What do you think is happening?”
- Do not suggest a control, command, or interpretation unless the participant requests help or the safety stop condition applies.
- Record every assistance event and classify it as clarification, navigation help, conceptual help, or direct instruction.
- Stop immediately if a participant is about to expose a credential, modify a non-fixture repository, or approve an operation outside the scripted scope.

## Metrics

For every task record:

- outcome: completed, completed with assistance, abandoned, blocked by product defect, or invalid run
- time on task
- wrong turns
- assistance count and type
- confidence before and after, from 1 to 5
- whether the participant can accurately explain the current execution state
- whether the participant can accurately explain why Medusa is blocked or requesting attention
- observed severity and reproducibility of usability failures

A wrong turn is an action that does not advance the task and requires reversal, recovery, or more than 30 seconds of investigation.

## Pre-registered success criteria

The initial study meets the minimum usability bar only when all conditions hold:

1. At least 5 of 6 participants complete installation/launch and provider onboarding without direct instruction.
2. At least 5 of 6 complete the bounded quickstart proof and identify whether verification passed.
3. At least 5 of 6 correctly distinguish active execution, waiting for approval, verification failure, and completed states.
4. Every participant declines or safely handles the out-of-scope approval variant.
5. At least 4 of 6 diagnose the scripted failed verification step without direct instruction.
6. At least 4 of 6 resume the interrupted session from the durable point.
7. At least 4 of 6 explain the rollback preview’s affected files and risks before confirming or cancelling.
8. At least 5 of 6 export the audit report and identify the task outcome, approval decision, and verification result.
9. No participant exposes credentials or modifies a non-fixture repository.
10. No critical usability failure remains without a reproducible follow-up issue.

These thresholds are a study gate, not a release claim. Results below threshold must be reported without reinterpretation.

## Task sequence

Run the tasks in this order:

1. Install or launch Medusa from a clean environment.
2. Configure a provider using the supported onboarding path.
3. Run the bounded quickstart proof.
4. Start the realistic repository task.
5. Explain the current execution and verification state.
6. Respond to the scripted sensitive approval request.
7. Diagnose the deterministic failed verification step.
8. Resume the interrupted session.
9. Inspect the checkpoint and rollback preview; confirm or cancel according to the task card.
10. Export and interpret the session audit report.

Do not skip a failed task. Record the failure, restore the prepared fixture when necessary, and continue with the next independently testable task.

## Severity rubric

- Critical: creates a credible risk of secret exposure, destructive action, false completion, unrecoverable work loss, or unsafe approval.
- High: prevents completion of a core workflow for multiple participants or causes a materially incorrect understanding of execution state.
- Medium: causes repeated delay, wrong turns, or assistance but has a discoverable workaround.
- Low: creates minor confusion or polish friction without changing task outcome.

Frequency:

- Systematic: observed in at least half of valid sessions
- Repeated: observed in two or more valid sessions
- Isolated: observed once

## Findings and issue policy

Create a follow-up issue for every critical or high-severity finding. Each issue must contain:

- protocol version and task number
- affected client and platform
- observed behavior and expected behavior
- sanitized reproduction steps
- frequency and severity
- evidence that does not contain participant data or repository secrets
- acceptance criteria

Group medium and low findings only when they share one root cause and one testable resolution.

## Study completion checklist

- participant count and cohort requirements satisfied
- at least two operating systems represented
- all valid sessions use this protocol version
- deviations documented
- raw data sanitized and stored according to consent
- findings report completed
- critical and high findings filed as issues
- no participant or repository-sensitive data committed
