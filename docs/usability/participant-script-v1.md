# Medusa Participant Script v1

Use with `protocol-v1.md`. Read quoted text verbatim. Text in facilitator notes is not read aloud.

## Opening and consent

> Thank you for helping us evaluate Medusa. We are testing the product, not you. Some tasks may be confusing or may fail. Please work as you normally would and think aloud when possible.
>
> We will record task outcomes, time, navigation, assistance, and your confidence ratings. We will not intentionally collect credentials, private source code, or personal information. Please stop immediately if you think a secret or non-test repository may be exposed.
>
> You may pause or end the session at any time. Do you consent to participate under these conditions?

Facilitator: record consent as yes or no. End the session if consent is not given.

## Background questions

Ask only:

1. Which operating system are you using?
2. Have you used a coding agent before? Never, occasionally, or regularly?
3. Are you familiar with Rust tooling? Yes or no?
4. Do you need any accessibility accommodation for this session?

Do not record employer, repository names, account identifiers, or credentials.

## Confidence scale

Before and after each task ask:

> On a scale from 1 to 5, how confident are you that you can complete this task, where 1 means not confident and 5 means very confident?

## Task 1 — Install or launch

> Starting from this clean environment, install or launch Medusa using the information available to you. Stop when you believe the application is ready for a task.

Completion evidence:

- supported client is running
- participant identifies that Medusa is ready or explains the blocking state correctly

Follow-up:

> What tells you that Medusa is ready?

## Task 2 — Provider onboarding

> Configure the prepared study provider using Medusa’s supported onboarding path. Do not paste or reveal any credential outside the designated secure input.

Facilitator: use a non-production study credential or mocked provider. Trigger the safety stop if a credential is exposed.

Follow-up:

> What information did Medusa retain, and what information do you believe remains secret?

## Task 3 — Bounded quickstart proof

> Run the quickstart task in the prepared repository. Stop when you believe the task has either completed or cannot continue.

Follow-up questions:

> What is the current task state?
>
> Did verification run, and what was its result?
>
> What evidence supports your answer?

Completion evidence:

- participant launches the bounded task
- participant identifies completion or blocking accurately
- participant distinguishes execution from verification

## Task 4 — Real repository task

> Start the prepared multi-file repository task. You may inspect Medusa’s activity and messages as needed. Stop when Medusa requests your attention or when you believe the task is complete.

Follow-up:

> Describe what Medusa has done so far and what remains.

Facilitator: do not explain internal agent phases or icons.

## Task 5 — Interpret execution state

Present the prepared state sequence: active execution, waiting for approval, failed verification, and completed.

For each state ask:

> What is happening now?
>
> Can Medusa continue without you?
>
> What should you do next, if anything?

Completion evidence: participant correctly identifies all four states and their required action.

## Task 6 — Sensitive approval

> Medusa is requesting approval for an operation. Review the request and decide whether to approve or reject it. Explain your decision.

The first request must be within the prepared task scope. The second variant must be visibly outside scope or request access beyond the fixture.

Follow-up:

> What would happen if you approved this?
>
> What information made the request safe or unsafe?

Safety success requires rejection or safe handling of the out-of-scope variant.

## Task 7 — Failed verification

> The task’s verification step has failed. Determine what failed, whether the implementation should be considered complete, and what the next safe action is.

Follow-up:

> Is the task complete? Why or why not?
>
> Which evidence is trustworthy, and which evidence is now uncertain?

Completion evidence:

- participant finds the failed check
- participant does not claim completion
- participant identifies a retry, fix, or escalation path

## Task 8 — Interrupted session and resume

Facilitator: use the prepared interrupted session.

> Open the interrupted session. Explain what Medusa knows about the last durable state, then continue the task from the safest available point.

Follow-up:

> What work is durable?
>
> What operation was interrupted?
>
> Which approvals, containment checks, or verification may need to be repeated?

Completion evidence: participant resumes from the durable continuation point or correctly explains why resume is blocked.

## Task 9 — Checkpoint and rollback preview

> Inspect the available checkpoint and preview the rollback or restore operation. Do not apply it until you can explain which files would change and what unresolved risks remain.

Use one clean-preview variant and one conflicting-local-work variant across the participant sample.

Follow-up:

> Which files would be affected?
>
> Would any uncommitted work be overwritten?
>
> Is confirmation required, and why?

Facilitator: the task card states whether the participant should confirm or cancel after explaining the preview.

## Task 10 — Audit export

> Export the session audit report. Use it to identify the task outcome, the approval decision, the recovery action, and the final verification result.

Follow-up:

> Could another developer understand what happened from this report alone?
>
> What important evidence is missing or unclear?

Completion evidence: participant exports the report and accurately identifies the requested facts.

## Closing interview

Ask:

1. Which moment felt least clear?
2. When did you feel least in control?
3. Which safety message or approval detail was most useful?
4. What information did you expect to find but could not?
5. What is the single most important improvement Medusa should make?

Final statement:

> Thank you. We will sanitize the observations before using them in product findings. No credential, private repository content, or personal identifier should be included in committed study artifacts.

## Facilitator observation row

For each task record:

- participant code
- cohort
- operating system
- client
- start and end time
- outcome
- wrong turns
- assistance type and count
- confidence before and after
- state explanation accurate: yes, partial, or no
- blocking reason explanation accurate: yes, partial, no, or not applicable
- sanitized observation
- suspected severity
- reproducible: yes, no, or unknown

Never use a participant’s name as the participant code.
