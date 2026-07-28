# Medusa Usability Recruitment and Sampling Plan v1

Issue: #421  
Protocol: `protocol-v1.md`  
Version: 1.0

## Purpose

Define how participants are recruited, screened, assigned, and counted so the study satisfies its pre-registered cohort and platform requirements without introducing avoidable sampling bias or collecting unnecessary personal information.

## Minimum valid sample

A study wave is valid only when it includes at least six completed, consented sessions and all of the following are represented:

- at least two first-time coding-agent users with no prior coding-agent use
- at least two experienced coding-agent users
- at least two operating systems
- both TUI and desktop clients where the tested build supports them
- at least one participant unfamiliar with Rust tooling

A participant may satisfy more than one requirement. Do not stop recruiting merely because six sessions are scheduled; continue until six valid sessions remain after exclusions.

## Cohort definitions

### First-time coding-agent cohort

A participant is classified as first-time only when they have never previously used a coding agent for a substantive coding task. At least two valid participants must meet this exact definition; near-first-time participants do not satisfy the frozen protocol minimum.

### Novice coding-agent cohort

A participant is classified as novice when they are first-time or have completed one or two substantive coding tasks with a coding agent. This broader novice label may be used for analysis, but only the zero-task subgroup satisfies the separate first-time minimum.

### Intermediate coding-agent cohort

A participant is classified as intermediate when they have completed three to nine substantive coding tasks with a coding agent. Intermediate participants may fill supplemental or reserve slots, but they satisfy neither the first-time nor the experienced minimum.

### Experienced coding-agent cohort

A participant is classified as experienced when they have completed at least ten substantive coding tasks with a coding agent.

### Rust familiarity

Record only `familiar` or `unfamiliar`. Familiarity means the participant can independently recognize common Cargo commands and Rust project structure. Rust familiarity is not used as a proxy for general development experience.

## Inclusion criteria

Participants must:

- be able to use one supported study operating system
- be able to complete a 60–90 minute remote or in-person session
- consent to the study terms
- agree to use only the disposable study fixture
- be able to think aloud or describe decisions after each task, with reasonable accommodation where needed

## Exclusion criteria

Exclude from the primary usability sample:

- Medusa maintainers or contributors who implemented the tested workflows
- participants who have seen the exact fixture tasks or participant script
- sessions run on an unsupported client/platform/scenario combination
- sessions with missing consent
- sessions where fixture self-check failed before the scored tasks
- sessions invalidated by secret exposure, non-fixture mutation, or facilitator protocol violation

Record exclusions by study code and reason. Do not record names in committed artifacts.

## Recruitment channels

Use more than one channel when practical to reduce selection bias. Suitable channels include:

- developers who have not contributed to Medusa
- coding-agent user communities
- general developer communities
- internal colleagues outside the Medusa project team
- participants with limited Rust exposure recruited through language-agnostic engineering groups

Avoid recruiting only highly enthusiastic coding-agent users. Do not advertise the study as a test of participant skill.

## Screening questions

Ask only the minimum needed for assignment:

1. Which operating systems can you use for the session?
2. How many substantive coding tasks have you previously completed with a coding agent: none, one or two, three to nine, or ten or more?
3. Are you familiar with Rust and Cargo tooling: yes or no?
4. Have you contributed code or design work to Medusa?
5. Have you previously seen the Medusa usability fixture or task script?
6. Do you need an accessibility accommodation for the session?

Map the second response deterministically: `none` to first-time and novice, `one or two` to novice, `three to nine` to intermediate, and `ten or more` to experienced.

Do not collect employer, exact job title, repository names, account identifiers, or credentials unless separately required and consented for study administration. Administrative contact details must remain outside the repository and must not appear in findings.

## Sampling matrix

Fill this before scheduling scored sessions.

| Slot | Cohort | Rust familiarity | Operating system | Client | Rollback variant | Status |
| --- | --- | --- | --- | --- | --- | --- |
| P01 | first-time | unfamiliar preferred | Linux/macOS/Windows | TUI/desktop | clean | open |
| P02 | first-time | any | different from P01 where practical | alternate client | conflict | open |
| P03 | experienced | any | supported | TUI/desktop | clean | open |
| P04 | experienced | any | supported | alternate client | conflict | open |
| P05 | novice/intermediate/experienced | any | fill platform gap | fill client gap | clean | open |
| P06 | novice/intermediate/experienced | any | fill platform gap | fill client gap | conflict | open |
| Reserve 1 | any defined cohort | any | any supported | any supported | either | open |
| Reserve 2 | any defined cohort | any | any supported | any supported | either | open |

Do not assign a participant to a client/platform combination that has not passed the fixture validation matrix.

## Assignment rules

- Balance clean and conflict rollback-preview variants across cohorts.
- Preserve at least two first-time assignments through completion and exclusions.
- Preserve at least two experienced assignments through completion and exclusions.
- Assign intermediate participants only to supplemental or reserve slots.
- Avoid assigning all novice participants to one client or operating system.
- Alternate task variants where multiple equivalent fixtures exist.
- Keep the participant script and success thresholds unchanged within a study version.
- Record assignment before the session begins.
- Do not move a participant between cohorts after observing task performance.

## Compensation and undue influence

When compensation is offered, state it before consent and do not condition payment on task completion, positive feedback, or finishing the full session. Participants who stop early should receive the promised compensation according to the recruitment terms.

Compensation details and payment identifiers remain outside the repository.

## Scheduling and reserve policy

Schedule at least two reserve participants because product defects, fixture failures, consent withdrawal, and safety stops can invalidate sessions.

A reserve session becomes part of the primary sample only when:

- its assignment preserves or improves cohort/platform coverage,
- the same protocol and fixture version are used,
- the session passes all validity gates.

## Recruitment tracking

Maintain a private administrative tracker outside the repository with:

- study code
- contact status
- scheduled time
- cohort assignment
- platform/client assignment
- consent status
- session validity
- compensation status, if applicable

Committed study artifacts may include only aggregate counts and anonymous study codes.

## Stop and extension rules

Do not declare the acceptance criteria met until six valid sessions are complete and all required cohorts are represented.

Extend recruitment when:

- fewer than six sessions remain valid,
- fewer than two valid first-time participants remain,
- fewer than two valid experienced participants remain,
- only one operating system is represented,
- a high-severity finding appears isolated to an unrepresented client/platform combination that can reasonably be tested,
- protocol deviations make a required task incomparable across sessions.

Do not silently replace excluded sessions or omit them from the validity section of the findings report.

## Reporting requirements

The aggregate findings report must state:

- number recruited
- number consented
- number completed
- number valid
- exclusions by reason
- first-time, novice, intermediate, and experienced cohort counts
- operating systems represented
- clients represented
- fixture and protocol versions used
- any sampling gaps or recruitment bias that limit conclusions