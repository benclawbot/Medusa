# Medusa Usability Session Run Checklist v1

Use with:

- `protocol-v1.md`
- `participant-script-v1.md`
- `fixture-spec-v1.md`
- `findings-template-v1.md`

This checklist is completed once per participant session. Store raw sheets outside the repository. Only sanitized aggregate findings may be committed.

## Session identity

- Participant study code:
- Cohort: novice / experienced
- Operating system and version:
- Client: TUI / desktop
- Medusa commit SHA:
- Client build identifier:
- Fixture version:
- Fixture commit SHA:
- Provider category: mock / recorded / disposable live
- Facilitator code:
- Session date:

Do not record participant names, email addresses, account identifiers, employer, or repository names.

## Pre-session validity gate

Complete before the participant joins.

- [ ] Dedicated disposable fixture path confirmed
- [ ] Fixture-root identity check passed
- [ ] Fixture reset completed from pinned commit
- [ ] Fixture self-check passed
- [ ] Quickstart verification passed
- [ ] Deterministic failure failed for the expected reason
- [ ] Interrupted-session artifact exists and is inspectable
- [ ] Clean and conflict rollback-preview variants are prepared
- [ ] Audit export directory is empty and writable
- [ ] Medusa build provenance recorded
- [ ] Planned client/platform/scenario combination is marked supported
- [ ] Mock or disposable provider is ready
- [ ] No real credential is present in task text, files, clipboard, or shell history
- [ ] Screen and notification environment contains no unrelated sensitive content
- [ ] Raw observation storage is outside the repository

If any item fails, do not begin the scored session.

## Consent gate

- [ ] Facilitator read the consent text verbatim
- [ ] Participant consented
- [ ] Optional recording consent captured separately, if applicable
- [ ] Participant knows they may stop at any time
- [ ] Participant knows to stop if a secret or non-fixture repository appears

No consent means the session ends and no scored data is retained.

## Assistance classification

Use exactly one category for each assistance event:

- **Clarification:** repeats or defines the task without revealing a control or answer.
- **Navigation help:** identifies where a control or view is located.
- **Conceptual help:** explains a product state or concept.
- **Direct instruction:** tells the participant which action to take.
- **Safety intervention:** stops credential exposure, non-fixture mutation, or an out-of-scope approval.

Safety interventions are always recorded but do not count as ordinary facilitator assistance.

## Task record

Complete this block for every scripted task.

- Task ID:
- Start time:
- End time:
- Confidence before, 1–5:
- Confidence after, 1–5:
- Outcome: completed / completed with assistance / abandoned / blocked by product defect / invalid run
- Wrong turns:
- Assistance events by category:
- State explanation: accurate / partial / incorrect / not applicable
- Blocking reason explanation: accurate / partial / incorrect / not applicable
- Verification interpretation: accurate / partial / incorrect / not applicable
- Sanitized observation:
- Suspected finding ID:
- Severity candidate: critical / high / medium / low
- Reproducible: yes / no / unknown

## Task-specific evidence

### Task 1 — Install or launch

- [ ] Participant reached a supported client state
- [ ] Participant correctly identified ready, onboarding-required, or blocked
- Evidence reference:

### Task 2 — Provider onboarding

- [ ] Supported onboarding path used
- [ ] No credential exposed in visible or exported evidence
- [ ] Participant described what remains secret
- Evidence reference:

### Task 3 — Bounded quickstart

- [ ] Task launched
- [ ] Expected fixture result produced
- [ ] Verification result visible
- [ ] Participant distinguished execution from verification
- Evidence reference:

### Task 4 — Multi-file task

- [ ] Participant described completed work
- [ ] Participant described remaining work
- [ ] Participant identified current verification status
- Evidence reference:

### Task 5 — Interpret execution state

Record each prepared state separately.

| State | Accurate | Partial | Incorrect | Assistance | Note |
| --- | --- | --- | --- | --- | --- |
| Active execution |  |  |  |  |  |
| Waiting for approval |  |  |  |  |  |
| Failed verification |  |  |  |  |  |
| Completed |  |  |  |  |  |

### Task 6 — Sensitive approval

- [ ] In-scope request decision recorded
- [ ] Participant explained consequences
- [ ] Out-of-scope request rejected, constrained, or safely blocked
- [ ] No external content exposed
- Evidence reference:

A failure to handle the out-of-scope variant safely is a high or critical finding depending on actual exposure or mutation risk.

### Task 7 — Failed verification

- [ ] Participant found the failed check
- [ ] Participant did not claim verified completion
- [ ] Participant identified a safe next action
- Evidence reference:

### Task 8 — Interrupted session and resume

- [ ] Participant identified last durable state
- [ ] Participant identified interrupted operation
- [ ] Participant understood checks or approvals that may need repetition
- [ ] Participant resumed safely or correctly explained why blocked
- Evidence reference:

### Task 9 — Checkpoint and rollback preview

- Variant: clean / conflict
- [ ] Participant identified affected files
- [ ] Participant identified overwrite or unresolved risks
- [ ] Participant understood confirmation requirement
- [ ] No mutation occurred before preview comprehension was assessed
- Evidence reference:

### Task 10 — Audit export

- [ ] Export completed
- [ ] Participant identified task outcome
- [ ] Participant identified approval decision
- [ ] Participant identified recovery action
- [ ] Participant identified final verification result
- [ ] Participant identified missing or unclear evidence
- Evidence reference:

## Safety-stop log

Complete for every safety intervention.

- Time:
- Trigger:
- Imminent risk:
- Facilitator action:
- Product behavior:
- Was any secret, non-fixture content, or external mutation exposed? yes / no / unknown
- Required containment or cleanup:
- Finding ID:

Any actual secret exposure or non-fixture mutation invalidates the session and requires incident handling outside the committed study artifacts.

## Protocol deviations

Record every deviation from the versioned protocol, script, fixture, or facilitation rules.

| Time | Deviation | Reason | Tasks affected | Scoring impact | Keep or exclude session? |
| --- | --- | --- | --- | --- | --- |
|  |  |  |  |  |  |

Do not silently normalize deviations during analysis.

## End-of-session checks

- [ ] Closing interview completed
- [ ] Participant reminded how data will be sanitized
- [ ] Raw export and observations stored outside repository
- [ ] Participant identifiers removed from working notes where unnecessary
- [ ] Credentials and provider data reviewed for accidental capture
- [ ] Screenshots or recordings reviewed before retention
- [ ] Fixture repository reset or destroyed
- [ ] Study credential revoked if disposable live credential was used
- [ ] Session validity decision recorded

## Session validity decision

- Decision: valid / valid with documented deviation / invalid
- Reason:
- Tasks excluded from aggregate scoring:
- Follow-up required:

Invalid sessions may inform qualitative debugging but must not be counted toward the pre-registered success thresholds.

## Finding escalation gate

Before closing analysis for this session:

- [ ] Every critical or high observation has a finding ID
- [ ] Reproduction steps are sanitized
- [ ] Frequency count has been updated
- [ ] Client and platform are recorded
- [ ] Safety/data implications are recorded
- [ ] A follow-up issue exists or is queued for immediate creation

## Sanitization review

A second reviewer should confirm before evidence is shared or committed:

- [ ] Participant code cannot be mapped from committed content
- [ ] No names, emails, usernames, machine names, or home paths remain
- [ ] No tokens, credentials, account IDs, or provider payloads remain
- [ ] No private or non-fixture source content remains
- [ ] Screenshots show only the intended fixture and product
- [ ] Evidence still supports the reported finding after redaction

Reviewer code:
Review date:
Result: approved / rejected
Notes: