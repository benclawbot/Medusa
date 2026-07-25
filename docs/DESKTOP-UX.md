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
- `DesktopTimelineBridge.tsx` projects the live plan and runtime activity into the central transcript without changing runtime event contracts.
- `desktop-timeline.css` styles the unified execution timeline and expandable activity details.

The bridge observes the already-rendered plan and activity state and mounts a React portal before the approval card or working indicator. This preserves the existing `App` ownership of polling, approvals, session lifecycle, commands, attachments, and settings while establishing the visual contract for a future direct shared-state timeline.

## Validation

The Desktop GitHub Actions workflow validates:

- npm dependency installation
- TypeScript type checking
- frontend tests
- production frontend build
- Rust adapter formatting, Clippy, panic audit, and tests on Linux, macOS, and Windows
- unsigned Linux, macOS, and Windows package builds

The Rust adapter matrix refreshes the dependency lock for the merged pull-request graph before running locked checks. This matches the workspace-quality workflow and prevents unrelated base-branch dependency changes from producing a stale nested lockfile failure.

## Follow-up architecture

A later refactor can replace DOM projection with a typed timeline model owned by `App` or a session store. That migration should preserve the current component-level visual contract:

- plan summary and connected steps
- chronological activity cards
- collapsed details by default
- approval cards in the central flow
- explicit active, completed, and failed states
