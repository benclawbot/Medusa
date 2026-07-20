# Durable Session Resume

Medusa persists repository-scoped interactive sessions and can continue them through the CLI, TUI, and desktop entry points without creating a replacement conversation.

## Preserved state

A resumed session keeps the same durable session identity and restores:

- objective and transcript messages;
- current turn number;
- plan and step progress;
- pending user question;
- completion state;
- evidence and verification chain;
- provider-neutral runtime state required for the next turn.

The runtime validates the stored session before starting. Invalid identifiers, missing sessions, repository mismatches, unsupported data, and failed integrity checks return an error instead of silently starting a new session.

## CLI and TUI

Resume an explicit session:

```bash
medusa --resume <session-id>
```

Resume the most recent compatible session:

```bash
medusa --continue
```

Headless continuation is also available:

```bash
medusa resume <session-id>
```

## Desktop

1. Open **Sessions** for the active repository.
2. Select a saved session to inspect its durable transcript.
3. Select **Resume this session**.
4. Medusa restarts the desktop runtime with the selected durable session.
5. The next prompt continues the restored objective, plan, pending question, transcript, and evidence chain.

The desktop uses a one-shot resume request. It is cleared during startup so later launches return to normal runtime creation unless another session is explicitly selected.

Desktop resume uses the same `RuntimeRegistry`, `RuntimeController`, daemon supervisor, polling, submission, cancellation, slash-command, attachment, model configuration, policy, and repository containment paths as a newly started desktop runtime.

## Storage and repository scope

Sessions are resolved only for the selected repository. Medusa checks the repository-local session directory and the platform fallback session directory associated with that canonical repository path. A session from another repository is not accepted merely because its identifier is known.

Session identifiers are treated as untrusted input and validated before filesystem lookup.

## Failure behavior

Resume is fail-closed:

- a missing or invalid session produces a visible error;
- corrupted or incompatible durable state is not replaced with a blank conversation;
- the active repository remains unchanged;
- no session data is deleted when resume fails.

The original durable file remains available for inspection or migration.

## Validation evidence

The live resume implementation was introduced through PRs #93 and #94. Before merge, the repository's required workflows passed:

- `CI`;
- `Desktop` on Linux, macOS, and Windows;
- `Daemon` on Linux, macOS, and Windows;
- `Refactor Guardrails`;
- `Release Gates`, including security, adversarial regressions, coverage, packaging, and live MiniMax scenarios.

The runtime API is implemented by `RuntimeController::start_resumed`. The desktop bridge is implemented in `apps/medusa-desktop/src-tauri/src/runtime_resume.rs`, with the session browser and resume action in `apps/medusa-desktop/src/SessionDock.tsx`.
