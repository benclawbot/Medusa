# Codex Maximum-Reliability Engineering Specification

Version: 1.0

This document defines the reliability operating model for Codex working on Medusa. The root [`AGENTS.md`](../AGENTS.md) is the enforceable repository instruction layer; this document explains the full workflow, review model, evidence standard, and task-completion gates.

---

## 1. Objective

Codex must optimize for, in order:

1. correctness;
2. demonstrated evidence;
3. minimal regression risk;
4. requirement coverage;
5. safe repository handling;
6. reproducible validation;
7. honest uncertainty.

Speed, brevity, confidence, and apparent completion are secondary.

The governing rule is:

> Reason about what requires inference. Inspect what can be inspected. Execute what can be tested. Verify what can be verified. Never claim more than the evidence supports.

A coherent explanation is not proof. Observable facts should be observed.

---

## 2. Epistemic model

Every material conclusion should internally be assigned one state:

### VERIFIED

Directly supported by one or more of:

- repository source inspected at the relevant revision;
- repository tests inspected at the relevant revision;
- executed local command output;
- compiler/linter/type-checker/formatter output;
- test output;
- authoritative CI output;
- official upstream documentation/source when external facts are required.

### INFERRED

Strongly supported by available evidence but not directly observed.

### UNKNOWN

Insufficient evidence.

Codex must never silently promote `INFERRED` or `UNKNOWN` to `VERIFIED`.

If a fact is required and checkable, check it. If it cannot be checked, report the limitation.

Reserved completion words such as `done`, `fixed`, `working`, `passing`, `resolved`, `merged`, and `closed` should only be used for directly observed states.

---

## 3. Default workflow

Every non-trivial coding task should move through:

```text
UNDERSTAND
    ↓
INSPECT
    ↓
SPECIFY
    ↓
PLAN
    ↓
REPRODUCE (for bugs when practical)
    ↓
IMPLEMENT
    ↓
TEST
    ↓
ADVERSARIAL REVIEW
    ↓
VERIFY
    ↓
REPORT
```

Do not skip stages merely because a solution looks obvious.

---

## 4. Task model

Before editing, build an internal task model:

```text
OBJECTIVE
CURRENT BEHAVIOR
REQUIRED BEHAVIOR
ACCEPTANCE CRITERIA
NON-GOALS
CONSTRAINTS
VALIDATION REQUIREMENTS
RISKS
UNKNOWN FACTS
```

The issue, user request, or accepted project specification defines required behavior unless stronger repository evidence demonstrates a contradiction that must be surfaced.

Do not silently narrow difficult requirements or broaden task scope.

---

## 5. Repository inspection

Before implementation, inspect the relevant subset of:

- `AGENTS.md` and any nested agent instructions;
- `CONTRIBUTING.md`;
- relevant README/docs;
- `Cargo.toml` and feature definitions;
- source files;
- direct callers and callees;
- interfaces and public APIs;
- existing tests;
- analogous implementations;
- platform-specific implementations;
- serialization/protocol/configuration code;
- CI workflows touching the affected component;
- security and containment boundaries.

The objective is to understand the execution path before changing it.

---

## 6. Requirement traceability

Translate the request into explicit acceptance criteria.

Example:

```text
AC1. Valid input produces the required output.
AC2. Invalid input returns the documented error.
AC3. Existing serialized state remains readable.
AC4. The operation cannot escape the repository boundary.
AC5. The reported regression is covered by a behavioral test.
```

Maintain an internal matrix:

| Requirement | Implementation evidence | Validation evidence | Status |
|---|---|---|---|
| AC1 | file/function | test/command | VERIFIED |
| AC2 | file/function | test/command | VERIFIED |
| AC3 | migration/compat path | compatibility suite | VERIFIED |
| AC4 | containment logic | behavioral proof | VERIFIED |
| AC5 | regression test | fails-before/passes-after | VERIFIED |

Any requirement without evidence remains unverified.

---

## 7. Planning

For non-trivial changes, plan before editing.

The plan should identify:

- files likely to change;
- functions/types likely to change;
- behavior being introduced or repaired;
- tests to add or update;
- required validation commands;
- backward-compatibility concerns;
- migration concerns;
- security/containment concerns;
- concurrency/lifecycle concerns;
- platform-specific concerns;
- cleanup/error-path concerns.

The plan is provisional. Repository evidence may invalidate it. Never force the codebase to fit an early hypothesis.

---

## 8. Minimal-change policy

Prefer the smallest coherent change that fully satisfies all acceptance criteria.

Avoid:

- unrelated refactors;
- speculative abstractions;
- opportunistic cleanup;
- public API expansion not required by the task;
- dependency changes not required by the task;
- broad renames;
- architecture rewrites without demonstrated need;
- formatting noise;
- changes to unrelated generated or lock files.

A smaller correct diff is easier to reason about, review, test, and revert.

---

## 9. Bug reproduction and root cause

For bugs, use this sequence whenever practical:

```text
1. Observe the failure.
2. Identify the execution path.
3. Determine the immediate cause.
4. Determine the root cause.
5. Add or identify a regression test.
6. Confirm it fails for the expected reason before the fix.
7. Implement the smallest root-cause fix.
8. Confirm the regression test passes.
9. Run adjacent tests.
10. Run broader required validation.
```

Internally, Codex should be able to answer:

```text
Observed failure:
Immediate cause:
Root cause:
Why current behavior is wrong:
Why the change addresses the root cause:
Why adjacent behavior remains correct:
```

If reproduction is impossible, record the reason and lower the verification level accordingly.

---

## 10. Implementation rules

Prefer:

- straightforward control flow;
- explicit invariants;
- existing project abstractions;
- narrow interfaces;
- deterministic behavior;
- typed APIs;
- explicit ownership;
- explicit cleanup;
- defensive handling of externally controlled input;
- local reasoning over hidden global coupling.

Avoid:

- broad exception/error swallowing;
- hidden global mutable state;
- timing-based synchronization where deterministic synchronization is possible;
- duplicated logic;
- speculative generalization;
- silent fallback behavior;
- magic constants without rationale;
- ambiguous ownership;
- undocumented platform assumptions.

Comments should explain non-obvious reasons, invariants, safety assumptions, protocol requirements, or platform constraints. Comments should not simply restate code.

---

## 11. Medusa-specific invariants

Changes must preserve applicable guarantees around:

- repository containment;
- process containment;
- sandbox behavior;
- rollback behavior;
- journal durability;
- credential redaction;
- migration correctness;
- serialized state compatibility;
- protocol compatibility;
- configuration compatibility;
- browser dispatch boundaries;
- provider credential isolation.

Protocol and configuration changes must follow [`PROTOCOL-VERSIONING.md`](PROTOCOL-VERSIONING.md).

Never commit provider credentials, generated `.medusa` state, build products, or local test artifacts.

User-visible commands, configuration, behavior, or compatibility changes require corresponding documentation updates.

---

## 12. Error handling

Errors should be:

- explicit;
- appropriately typed when the subsystem supports typed errors;
- contextual enough to diagnose;
- propagated or handled intentionally;
- testable when materially relevant.

Do not:

- swallow errors;
- convert an error into success;
- replace a specific diagnostic with a vague one without necessity;
- introduce fallback behavior that masks corrupt or invalid state.

Failure paths require the same lifecycle scrutiny as success paths.

---

## 13. Security review

For changes crossing trust boundaries, explicitly inspect applicable risks:

- untrusted input validation;
- path traversal;
- symlink and canonicalization behavior;
- command execution;
- shell quoting;
- environment-variable inheritance;
- secret/credential exposure;
- authentication and authorization;
- filesystem and repository boundaries;
- network boundaries;
- sandbox and process containment;
- privilege transitions;
- unsafe Rust / FFI boundaries;
- deserialization and parser abuse;
- injection;
- temporary files;
- cleanup after partial failure;
- denial-of-service amplification;
- resource exhaustion.

Security assumptions should be converted to behavioral evidence whenever practical.

---

## 14. Concurrency and async review

For concurrent, asynchronous, or multi-process code inspect:

- data races;
- logical races;
- deadlocks;
- ordering guarantees;
- atomicity;
- cancellation;
- retries;
- idempotency;
- partial failure;
- shared mutable state;
- timeout semantics;
- task/process lifecycle;
- child/grandchild termination;
- lock lifetime;
- handle lifetime;
- cleanup on all exits.

A happy-path test is insufficient evidence for lifecycle-sensitive code.

---

## 15. Resource-lifecycle review

For files, sockets, processes, threads, tasks, locks, temporary directories, handles, transactions, and similar resources, verify:

- creator/owner;
- transfer of ownership;
- normal cleanup;
- error cleanup;
- cancellation cleanup;
- timeout cleanup;
- process-tree cleanup where applicable;
- no stale state after rollback.

---

## 16. Platform review

Do not infer behavioral parity across supported operating systems.

When relevant, inspect Linux/macOS/Windows differences in:

- path syntax and normalization;
- permissions and ACLs;
- process creation;
- process termination;
- signals/job objects;
- filesystem semantics;
- environment inheritance;
- socket/network behavior;
- sandbox capabilities;
- packaging;
- executable lookup;
- temporary-directory behavior.

Consequential platform claims require platform-specific evidence.

---

## 17. Testing policy

Tests should prove externally meaningful behavior rather than mirror implementation details.

For new behavior, consider applicable cases:

- nominal behavior;
- lower/upper boundary behavior;
- malformed input;
- unauthorized input;
- unavailable dependency;
- failure halfway through the operation;
- rollback;
- cleanup;
- concurrency;
- cancellation;
- timeout;
- compatibility with existing state;
- platform-specific behavior;
- containment/security behavior.

Do not add trivial tests merely to increase coverage.

Never weaken assertions solely to make new code pass.

---

## 18. Regression-test rule

Every bug fix should have a behavioral regression test whenever practical.

Ideal proof:

```text
before fix: regression test FAILS for the expected reason
after fix:  regression test PASSES
```

If a pre-fix failure cannot be demonstrated because the environment or platform is unavailable, record the limitation explicitly.

---

## 19. Failure collection

When a command, suite, or CI run exposes several failures:

1. gather all available failures;
2. classify failures by subsystem;
3. identify likely primary versus cascading failures;
4. look for shared root causes;
5. repair root causes rather than symptoms;
6. rerun targeted validation;
7. rerun broader validation.

Do not enter a blind one-error/one-patch loop if the available evidence can reveal the full failure set.

---

## 20. Flaky-test policy

A flaky result lowers confidence.

Do not:

- blindly rerun until green;
- increase timeouts without evidence;
- disable a flaky test;
- reduce assertions;
- classify a rerun pass as proof the first failure was harmless.

Investigate whether the task introduced timing sensitivity, nondeterministic state, race conditions, resource exhaustion, or ordering changes.

If instability remains unexplained, report it.

---

## 21. Validation ladder

Use progressive validation for fast feedback, then complete required validation.

Typical sequence:

```text
1. syntax / parser checks
2. formatter
3. static analysis / lint
4. type checking / compilation
5. targeted unit tests
6. targeted integration tests
7. affected package/crate tests
8. workspace tests
9. documentation build
10. dependency/security policy checks
11. platform-specific tests
12. CI-equivalent/certification checks
```

For Medusa, the documented core checks are:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-features --locked
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --locked --no-deps
cargo deny check advisories sources
cargo audit
```

The release workflows may additionally enforce coverage, named containment/rollback regressions, fuzz, migration, chaos, package smoke tests, browser certification, Windows-specific checks, and credential-gated provider tests.

Run task-specific checks required by affected components in addition to the core suite.

---

## 22. Validation evidence

For every significant check, retain the conceptual tuple:

```text
COMMAND
EXIT STATUS
RELEVANT OUTPUT
INTERPRETATION
```

Never state that a command passed unless it completed successfully.

Never state that the full suite passed if only a subset was executed.

Never treat absence of output as success unless the command's semantics justify that interpretation.

---

## 23. Final diff review

Before completion, inspect the final diff and repository status.

Look for:

- accidental edits;
- unrelated edits;
- debug statements;
- temporary code;
- dead code;
- stale comments;
- generated artifacts;
- credential material;
- lockfile changes;
- dependency changes;
- formatting-only noise;
- weakened tests;
- disabled checks;
- unexplained TODOs;
- file-permission changes;
- unnecessary complexity.

A green test suite does not replace diff inspection.

---

## 24. Adversarial review

After implementation, stop reasoning like the author.

Assume the implementation is subtly wrong and attempt to falsify it.

Review against the original task, not the implementation plan.

Questions:

```text
Which acceptance criterion might still be unmet?
Which important assumption was never verified?
What boundary input breaks the behavior?
What happens when a dependency fails?
What happens after partial progress?
What happens on cancellation or timeout?
What happens during cleanup?
What happens concurrently?
What happens on Linux/macOS/Windows?
What compatibility promise could have changed?
What security boundary might be weaker?
Could these tests pass while the real requirement is still wrong?
Did the implementation change more than necessary?
```

Resolve material findings before completion.

---

## 25. Independent-review architecture

Where Codex orchestration or subagents are available, prefer independent roles:

```text
INVESTIGATOR
read-only
    ↓
PLANNER
read-only
    ↓
IMPLEMENTER
write access
    ↓
TEST HARNESS
    ↓
REVIEWER
read-only
    ↓
VERIFIER
read-only
```

The investigator gathers evidence without editing.

The planner derives the smallest implementation strategy and acceptance mapping.

The implementer receives the requirements and evidence, performs the change, and validates it.

The reviewer should receive the original task, repository, final diff, and test evidence. Prefer not to preload the implementer's confidence statements or detailed rationale before independent review.

The verifier decides whether completion gates are actually proven.

---

## 26. Multi-hypothesis reasoning

For difficult design decisions or subtle debugging, prefer independent hypotheses over repeated self-reconsideration.

Recommended pattern:

```text
Agent A → independent diagnosis/solution
Agent B → independent diagnosis/solution
Agent C → attack both and identify missing evidence
Verifier → adjudicate using repository/test evidence
```

Disagreement is useful when it exposes assumptions.

---

## 27. Reasoning-effort allocation

If model/effort is configurable, allocate compute by uncertainty and consequence:

```text
formatting / trivial edits        low
mechanical bounded changes        medium
repository exploration            high
normal implementation             high/xhigh
difficult debugging               xhigh
architecture                      xhigh/max
adversarial review                xhigh/max
high-risk final verification      strongest available model/effort
```

Do not assume higher effort alone guarantees correctness. Use repository-specific evals to determine the best configuration.

---

## 28. Context management

Large context capacity is not permission to include irrelevant history.

Prefer a compact evidence bundle:

```text
task
+ agent/repository instructions
+ acceptance criteria
+ relevant architecture
+ relevant source
+ relevant tests
+ necessary external documentation
```

Exclude obsolete plans, irrelevant conversation history, repeated logs, and unrelated files.

Fresh context is preferred for an independent reviewer/verifier.

---

## 29. External information

When implementation depends on current external behavior, verify it using primary sources.

Examples:

- crate/API versions;
- operating-system behavior;
- provider API behavior;
- CI action behavior;
- dependency security guidance;
- GitHub issue/PR status;
- release compatibility.

Preferred evidence order:

1. repository source/tests;
2. official upstream documentation;
3. upstream source;
4. upstream changelog/release notes;
5. strong secondary sources.

Version-sensitive claims should not rely on model memory when direct verification is possible.

---

## 30. Tool-use policy

Observable facts should be obtained with tools.

Examples:

```text
Does file X contain behavior Y?
→ inspect X.

Does this compile?
→ compile it.

Are tests passing?
→ run them or inspect authoritative CI.

Is the issue still open?
→ inspect GitHub.

Did the PR merge?
→ inspect GitHub.
```

Reasoning predicts. Execution verifies.

---

## 31. Git safety

Before consequential Git operations, inspect branch/status/diff.

Never:

- discard unrelated changes;
- overwrite user work;
- reset unrelated files;
- force-push without explicit authorization;
- rewrite history unnecessarily;
- delete branches unexpectedly;
- mix unrelated work into the task commit.

Commit scope should match task scope.

Before committing, verify no secrets, build artifacts, generated `.medusa` state, or unrelated files are included.

---

## 32. Pull-request policy

Before creating or updating a PR:

- verify branch state;
- inspect the final diff;
- run applicable validation;
- accurately describe the problem and intended behavior;
- summarize implementation approach;
- list exact tests/checks run;
- disclose security/migration/platform/rollback impact;
- disclose anything not verified.

Never claim a PR was created or updated unless the operation succeeded.

---

## 33. CI policy

CI is authoritative evidence about the environment it actually ran in, but it does not replace local reasoning.

When CI fails:

1. inspect every available failing job;
2. inspect available logs/errors;
3. classify failures;
4. determine whether failures are task-related, cascading, flaky, or infrastructure-related;
5. fix all task-related root causes;
6. rerun/observe new CI results;
7. do not describe CI as green until required checks are actually green.

---

## 34. Merge policy

Merge only when all applicable gates hold:

```text
requirements = VERIFIED
required local validation = PASS
required CI = PASS
review findings = RESOLVED
branch/PR state = VERIFIED
merge authorization = PRESENT
```

Use an expected head SHA when supported so the merge cannot silently apply to a different revision.

After merge, verify the resulting merged state.

If the task is associated with an issue, verify whether the issue was automatically closed. Do not assume closure.

---

## 35. One-issue-at-a-time workflow

When operating Medusa issues sequentially:

```text
1. Verify the selected issue is open.
2. Verify whether an existing PR already addresses it.
3. Read issue comments and linked constraints.
4. Derive acceptance criteria.
5. Create isolated branch/worktree.
6. Reproduce the problem where practical.
7. Implement the smallest correct fix.
8. Add regression evidence.
9. Run targeted validation.
10. Run required repository validation.
11. Inspect final diff.
12. Create/update PR.
13. Inspect all CI failures.
14. Fix all task-related failures.
15. Re-run/observe required CI.
16. Merge only when authorized and all gates pass.
17. Verify merge.
18. Verify related issue closure.
19. Only then move to the next issue.
```

Do not accumulate unrelated issue work into one branch or PR unless explicitly requested.

---

## 36. Failure-budget rule

Any failed validation command changes the task state to unverified until the failure is explained and resolved.

Do not:

- patch blindly;
- skip the failing check;
- weaken the check;
- hide the error;
- treat partial success as completion.

Do:

- gather evidence;
- classify failures;
- identify root cause;
- repair root cause;
- rerun affected checks;
- rerun broader checks as necessary.

Repeated unexplained failures should trigger deeper investigation rather than increasingly speculative patches.

---

## 37. Uncertainty thresholds

Use this policy:

```text
HIGH CONFIDENCE + DIRECT EVIDENCE
→ proceed

MEDIUM CONFIDENCE
→ inspect / search / test

LOW CONFIDENCE
→ investigate before consequential modification

UNKNOWN + CHECKABLE
→ check

UNKNOWN + NOT CHECKABLE
→ disclose explicitly
```

Do not invent missing facts to preserve momentum.

---

## 38. Blocked state

Report `BLOCKED` instead of fabricating progress when, for example:

- required credentials are unavailable;
- a required external service cannot be reached;
- a required platform cannot be exercised;
- repository state is unsafe to modify;
- authorization is required for a destructive/merge action;
- required information cannot be obtained;
- validation cannot be completed.

Report:

```text
BLOCKER:
WHY IT MATTERS:
WHAT WAS VERIFIED:
WHAT REMAINS UNKNOWN:
```

A blocker should be precise and evidence-backed.

---

## 39. Completion gate

A task may be marked `VERIFIED` only if every applicable condition is true:

```text
requirements mapped
AND implementation complete
AND targeted tests pass
AND broader relevant tests pass
AND formatter passes
AND linter/static analysis passes
AND compiler/type/documentation checks pass
AND dependency/security policy checks pass
AND platform checks pass where required
AND final diff reviewed
AND no unexplained changes remain
AND regression behavior is proven when applicable
AND adversarial review findings are resolved
AND remaining uncertainty is disclosed
```

If an applicable requirement is missing, use `PARTIALLY VERIFIED`, `NOT VERIFIED`, or `BLOCKED`.

---

## 40. Final report format

For non-trivial coding work:

```text
STATUS
VERIFIED / PARTIALLY VERIFIED / NOT VERIFIED / BLOCKED

CHANGES
- Material behavior changed
- Material files changed

REQUIREMENTS
- AC1: VERIFIED
- AC2: VERIFIED
- AC3: NOT VERIFIED

VALIDATION
- command → PASS
- command → PASS
- command → FAIL / NOT RUN

REVIEW
- Final diff inspected
- Relevant risk classes checked
- Material findings resolved

REMAINING UNCERTAINTY
- None
or
- Explicit limitation
```

Skipped checks must remain visible.

---

## 41. Prohibited reliability failures

Codex must not:

- hallucinate file contents;
- hallucinate command output;
- hallucinate test results;
- hallucinate CI state;
- hallucinate issue/PR state;
- claim execution that did not occur;
- assume an API exists without evidence when it can be checked;
- invent repository conventions;
- claim platform support without evidence;
- mark incomplete work done;
- weaken tests to get a green run;
- suppress errors without justification;
- silently broaden the task;
- silently omit requirements;
- overwrite unrelated user work;
- substitute confidence for verification.

---

## 42. Agent-quality evals

Maintain a set of historical representative Medusa tasks and measure candidate model/effort configurations.

Track:

- acceptance-criterion coverage;
- regressions introduced;
- hidden-test pass rate where available;
- review defects;
- false completion claims;
- hallucinated repository/CI states;
- unnecessary diff size;
- retries and CI cycles;
- time/tokens/cost;
- model and reasoning effort.

Use measured repository performance rather than intuition to choose model and effort settings.

Useful comparisons may include:

```text
model A / high
model A / xhigh
model A / max
model B / high
model B / xhigh
model B / max
```

The objective is reliable task completion, not maximum reasoning tokens in isolation.

---

## 43. Compact Codex contract

The following is the minimal operational contract:

```text
Correctness and evidence take priority over speed or apparent completion.

Before editing:
- derive acceptance criteria;
- inspect repository instructions;
- inspect relevant implementation, callers, tests, and analogous code;
- identify unknown assumptions;
- reproduce bugs when practical.

During implementation:
- make the smallest coherent change;
- preserve unrelated behavior;
- avoid speculative refactoring;
- add behavioral regression tests for bugs;
- handle errors, cleanup, concurrency, security, and platform behavior explicitly.

During validation:
- gather all available failures before patching;
- run targeted tests first, then all applicable repository-required checks;
- never claim a test, build, CI job, issue state, PR state, or platform behavior without direct evidence;
- inspect the final diff.

Before completion:
- adversarially attempt to disprove the implementation;
- map every acceptance criterion to implementation and validation evidence;
- disclose anything not verified.

If required evidence is missing, report NOT FULLY VERIFIED instead of inferring success.
```

---

## 44. Reliability principle

The best Codex configuration is not one that merely spends more reasoning tokens.

It is one in which unsupported claims cannot satisfy completion gates.

The intended progression is:

```text
specification
→ observation
→ implementation
→ executable proof
→ adversarial review
→ independent verification
→ completion
```

Reasoning effort strengthens this process; it does not replace it.
