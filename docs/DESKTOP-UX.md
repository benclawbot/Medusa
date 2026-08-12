# Medusa Desktop UX

Medusa Desktop follows an activity-first interaction model: the conversation is the primary surface, while execution detail remains visible without competing with the user's task.

## Design principles

1. **Conversation first** — prompts, responses, approvals, plan state, and tool activity stay in one chronological workspace.
2. **Progressive disclosure** — summaries remain scannable; detailed tool output is expandable.
3. **Visible execution** — active work, completion, failure, and plan progress have distinct states.
4. **Stable controls** — the composer, cancellation, attachments, slash commands, and approval actions remain predictable while work is running.
5. **Low cognitive load** — the inspector summarizes session context and usage instead of duplicating execution details.
6. **Responsive focus** — narrower windows remove secondary chrome before shrinking the primary conversation.
7. **Accessible motion** — progress animation is subtle and disabled when reduced motion is requested.

## Current implementation

The desktop shell uses two isolated presentation layers:

- `desktop-ux-overhaul.css` defines the premium visual hierarchy, message treatment, composer, plan tree, activity cards, inspector, and responsive focus mode.
- `DesktopTimelineBridge.tsx` renders the live plan and runtime activity into the central transcript from the typed runtime timeline store.
- `desktop-timeline.css` styles the unified execution timeline and expandable activity details.

`runtime.ts` reduces the same typed `RuntimeEvent` values consumed by `App` into a small external timeline snapshot. The timeline subscribes with `useSyncExternalStore`, so plan, activity, and working state no longer depend on DOM text extraction or a document-wide `MutationObserver`. The portal remains only as a layout boundary that places execution state inside the transcript without changing approval, command, attachment, or session contracts.

## Timeline state rules

- a new runtime or `/new` session resets plan, activity, and busy state
- activity entries with stable IDs update in place instead of duplicating
- plan events replace the current typed plan snapshot
- start events set the active state
- questions, completion, cancellation, turn completion, and failures clear the active state
- the central timeline keeps only the latest twelve activity cards visible while the inspector retains its existing summary behavior

## Validation

The Desktop GitHub Actions workflow validates:

- npm dependency installation
- TypeScript type checking
- frontend tests
- production frontend build
- Rust adapter formatting, Clippy, panic audit, and tests on Linux, macOS, and Windows
- unsigned Linux, macOS, and Windows package builds

The Rust adapter matrix refreshes the dependency lock for the merged pull-request graph before running locked checks. This matches the workspace-quality workflow and prevents unrelated base-branch dependency changes from producing a stale nested lockfile failure.

The timeline store has focused frontend tests covering typed event reduction, stable activity replacement, busy-state completion, subscriber notification, and runtime reset behavior.

## Remaining UX validation

The visual contract is now backed by typed state. Remaining UX work is verification and polish rather than timeline architecture:

- headed end-to-end sessions on Linux, macOS, and Windows
- keyboard-only and screen-reader passes across timeline, approvals, and composer
- dense-session testing with long plans, large tool details, failures, and hundreds of events
- onboarding, empty-state, settings, and session-history consistency
