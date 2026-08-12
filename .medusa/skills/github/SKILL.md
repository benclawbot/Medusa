---
name: github
description: Use Medusa's typed GitHub capability from the runtime, TUI, or desktop with the same safety, confirmation, and audit rules.
---

# GitHub parity workflow

Use this skill when the user invokes `/github` or asks the runtime/TUI to inspect or mutate GitHub state.

## Capability gate

1. Check the shared `GitHub` runtime capability before doing work.
2. Validate `gh auth status`, the active account, hostname, and required scopes.
3. Verify repository visibility, default branch, and effective permissions before any repository operation.
4. Never request, print, persist, summarize, or place authentication tokens in prompts, transcripts, logs, memory, previews, or audit records.

## Typed operation matrix

Route each request to the first-class GitHub operation rather than assembling shell commands ad hoc:

- repository: clone, fetch, pull, branch listing, branch checkout, commit inspection
- pull requests: create draft, update title/body, inspect, review, close, and merge
- issues: list, create, comment, assign, label, milestone, and update
- checks and Actions: inspect checks, inspect workflow/job logs, watch runs, retry failed jobs, and cancel runs
- merge: require the exact pull-request number, expected head SHA, merge method, and target branch

Use shell-free argument execution. Do not use force push, history rewriting, branch deletion, or destructive Git flags.

## Read operations

For reads, return a structured result with:

- repository and hostname
- active account when available
- operation name
- normalized resource identifiers
- deterministic ordering
- safe HTTPS URLs only
- a recovery message for missing authentication, scopes, or access

Do not copy raw stderr into the user-visible result.

## Mutation preview

Before every externally visible write, present one preview containing:

- operation kind
- repository and hostname
- active account
- branch or expected head SHA
- recipients, reviewers, or assignees
- title and body summary
- affected resources
- whether the action is destructive

Create a deterministic fingerprint from the normalized preview. The confirmation must contain that exact fingerprint and a confirmation timestamp.

## Confirmation rules

- Non-destructive writes require explicit confirmation of the active preview.
- Destructive writes require an explicit destructive confirmation.
- Reject stale confirmation when repository, branch, head SHA, recipients, title/body, or affected resources changed.
- Reject merge when the pull request is closed, still a draft, or its head SHA differs from the confirmed SHA.
- Never infer confirmation from an earlier unrelated message.

## Durable audit evidence

After every mutation, emit and persist a secret-safe audit record containing:

- operation
- repository
- normalized resource identifiers
- preview fingerprint
- confirmation timestamp
- outcome
- resulting URL or commit SHA when safe

The audit record must not contain tokens, raw command output, raw stderr, environment variables, or credential-bearing URLs.

## TUI behavior

In the TUI, show reads as notices and mutations as a preview followed by a confirmation question. On rejection or failure, keep the original request recoverable and display an actionable, secret-safe explanation. Do not execute a mutation until the confirmation answer is received.

## Examples

- `/github inspect checks for HEAD`
- `/github list open issues`
- `/github retry failed job 123 from run 456`
- `/github update pull request 42 title to Fix startup race`
- `/github merge pull request 42 at head abcdef1 using squash`
