# Medusa Usability Study Fixture Specification v1

Protocol: `protocol-v1.md`  
Participant script: `participant-script-v1.md`  
Issue: #421  
Version: 1.0

## Purpose

Define a reproducible, disposable study environment for the task-based usability protocol. The fixture must let participants exercise first-run setup, execution-state comprehension, approvals, failed verification, interruption and recovery, rollback preview, and audit export without exposing credentials or private repositories.

## Fixture principles

- Use only synthetic or explicitly licensed test content.
- Make every expected result objectively checkable.
- Keep all destructive operations inside the disposable fixture root.
- Never require access to a participant's personal repository, account, or production credential.
- Pin every repository, scenario, client build, and provider configuration used in a session.
- Prefer deterministic offline behavior; clearly mark optional live-provider variants.
- Reset the fixture from a known commit before every valid session.

## Required fixture inventory

The study kit must include the following independently resettable scenarios.

| ID | Scenario | Required product behavior | Objective completion evidence |
| --- | --- | --- | --- |
| F-01 | Clean launch | Start a supported Medusa client from a clean environment | Client reaches ready, blocked, or actionable onboarding state without crash |
| F-02 | Provider onboarding | Configure the prepared mock or study provider | Provider is available and no secret appears in logs, transcript, or export |
| F-03 | Bounded quickstart | Execute a small deterministic repository task | Expected file change exists and the configured verification passes |
| F-04 | Multi-file task | Run a realistic change touching several fixture files | Participant can explain current work, pending work, and verification state |
| F-05 | Sensitive approval | Present one in-scope and one out-of-scope operation | Participant can distinguish scope and safely reject or constrain the unsafe request |
| F-06 | Failed verification | Produce a deterministic failing check after plausible implementation work | Product shows failure and does not imply verified completion |
| F-07 | Interrupted session | Stop after a durable checkpoint during active work | Session can be reopened and last durable state is inspectable |
| F-08 | Rollback preview | Provide clean and conflicting-local-work variants | Preview names affected files and unresolved overwrite risks before mutation |
| F-09 | Audit export | Export one completed or recovered session | Report identifies actions, approvals, recovery, and final verification evidence |

## Repository layout

Use a small synthetic repository with at least:

```text
study-fixture/
├── README.md
├── app/
│   ├── formatter.*
│   └── parser.*
├── tests/
│   ├── formatter_test.*
│   └── parser_test.*
├── scripts/
│   ├── verify-quickstart.*
│   ├── verify-multifile.*
│   └── fail-deterministically.*
├── generated/
│   └── sample-output.*
└── study/
    ├── expected/
    ├── scenarios/
    └── reset.*
```

The language and build tool may vary, but the fixture must run on at least two target operating systems without requiring participants to understand Rust tooling.

## Scenario definitions

### F-01 — Clean launch

Prepare a clean user-data directory and an empty workspace entry point. Record:

- Medusa commit SHA
- client build identifier
- operating system and version
- launch command or installer version
- whether prior Medusa configuration exists

Pass condition: the participant reaches a product state they can correctly identify as ready, onboarding-required, or blocked with an actionable reason.

### F-02 — Provider onboarding

Use one of:

1. a local mock provider with no secret,
2. a disposable study credential injected only through the supported secure input,
3. a recorded deterministic provider adapter approved for usability testing.

Never place a credential in task text, shell history, fixture files, screenshots, or committed artifacts.

Pass condition: the provider is configured through the supported path and no secret appears in exported evidence.

### F-03 — Bounded quickstart

Task prompt:

> Update the fixture's greeting formatter so names are trimmed and empty names produce the documented fallback. Run the relevant verification.

Expected change:

- one implementation file
- one or two focused tests
- deterministic verification under five minutes on supported study machines

Pass condition: expected behavior and verification result are both visible to the participant.

### F-04 — Multi-file task

Task prompt:

> Add a small structured-output option to the fixture parser, update tests and documentation, and preserve the existing default behavior.

Requirements:

- touches at least three files
- includes a meaningful intermediate state
- produces several execution and verification events
- does not require network access

Pass condition: the participant can accurately describe what changed, what remains, and whether verification is current.

### F-05 — Sensitive approval

Create two approval cards:

- **In scope:** run the fixture's local verification command inside the repository.
- **Out of scope:** read a clearly external path or invoke a network operation not needed by the task.

The unsafe variant must be simulated or safely denied by containment. It must never expose actual external content.

Pass condition: all participants handle the out-of-scope request safely; approval rationale is captured without recording sensitive data.

### F-06 — Failed verification

Provide a deterministic command that exits non-zero with a clear, fixture-owned diagnostic. The failure must not depend on timing, network, rate limits, or external services.

Pass condition: Medusa displays the failed verification distinctly from execution completion, and the participant does not report the task as verified.

### F-07 — Interrupted session

Create the interruption only after a durable checkpoint and before final verification. Record:

- session identifier
- checkpoint identifier
- last durable task step
- interrupted operation
- expected approvals or containment checks to re-establish

Pass condition: reopening the session surfaces the durable point and does not claim uncertain work is verified.

### F-08 — Rollback preview

Prepare two variants:

- **Clean:** working tree still matches the checkpoint base.
- **Conflict:** a fixture file contains an uncommitted participant-independent edit after the checkpoint.

Pass condition: preview identifies affected files; the conflict variant warns about overwrite risk and requires explicit confirmation or cancellation.

### F-09 — Audit export

The expected report must include enough evidence to identify:

- task and session identifiers
- important state transitions
- approval request and decision
- interrupted state and selected recovery action
- verification attempts and final result
- Medusa and fixture commit identifiers

Pass condition: the participant can answer the script's audit questions from the export alone.

## Reset contract

Before each participant:

1. verify the fixture remote and pinned commit,
2. remove untracked files inside the disposable fixture root,
3. restore the scenario-specific branch or snapshot,
4. reset Medusa study configuration to the planned clean or returning-user state,
5. delete prior session identifiers and exports,
6. run a fixture self-check,
7. record the resulting hashes in the session sheet.

Never run reset commands against a path that has not passed the fixture-root identity check.

## Fixture self-check

A valid kit provides one command that verifies:

- required files and scenarios exist
- expected fixture commit is checked out
- quickstart verification passes before the session
- deterministic failure fails for the intended reason
- interrupted-session and checkpoint artifacts are present
- no credential-like values are stored in fixture files
- output and export directories are writable

A failed self-check invalidates the session until corrected.

## Platform matrix

Before recruitment begins, record support for each scenario:

| Scenario | Linux | macOS | Windows | TUI | Desktop |
| --- | --- | --- | --- | --- | --- |
| F-01 |  |  |  |  |  |
| F-02 |  |  |  |  |  |
| F-03 |  |  |  |  |  |
| F-04 |  |  |  |  |  |
| F-05 |  |  |  |  |  |
| F-06 |  |  |  |  |  |
| F-07 |  |  |  |  |  |
| F-08 |  |  |  |  |  |
| F-09 |  |  |  |  |  |

Do not assign a participant to an unvalidated scenario/client/platform combination.

## Artifact handling

Raw artifacts remain outside the repository. Before any evidence is linked or committed:

- replace participant names with study codes
- remove usernames, home paths, emails, tokens, account IDs, and machine names
- remove non-fixture source content
- redact provider request and response bodies unless generated by the mock fixture
- verify screenshots contain no unrelated applications or notifications
- preserve timestamps only when needed for task-duration evidence

Committed fixture content must contain no participant-derived data.

## Change control

After the first participant session, changes to prompts, expected outputs, failure modes, or scoring-relevant behavior require:

- a new fixture version,
- a documented protocol deviation,
- explicit identification of which sessions used each version.

Do not silently repair a scenario in place and combine results across materially different fixture versions.