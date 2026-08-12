# Medusa repository instructions

Place this file at the repository root. These instructions apply to the entire repository.

## How to work

- Answer simple questions directly. Do not inspect the repository or invoke tools unless the answer depends on repository contents.
- For implementation work, inspect only the relevant files, make the smallest coherent change, and verify the behavior.
- Use `fs_list` for directory listings. Follow `next_cursor` until `complete` is `true` before claiming a listing is complete.
- Never invent file contents, command output, test results, or completion status.
- Do not retry the same blocked operation through equivalent shells or interpreters. After two equivalent failures, state the limitation and use a supported tool.
- Use a visible plan for work that requires multiple meaningful steps. Keep it current while working.
- Ask a blocking question only when missing information genuinely prevents progress. Otherwise make a reasonable, explicit assumption.

## Code quality

- Keep Rust changes focused, idiomatic, and cross-platform unless a file is explicitly platform-specific.
- Preserve public APIs unless the requested change requires an API update.
- Do not weaken security boundaries, path confinement, approval checks, or sandboxing to make a task easier.
- Do not modify tests, fixtures, snapshots, verification scripts, or expected outputs merely to hide a product defect.
- Add or update tests for behavior changes and regressions.
- Prefer structured errors and explicit state over string matching.

## Validation

Run the narrowest relevant checks first, followed by the workspace checks when available:

```text
cargo fmt --all -- --check
cargo test -p <affected-crate>
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-targets --all-features
```

If a check cannot run, report the exact command and the concrete environmental blocker. Never describe an unexecuted check as passing.

## Completion report

Include:

1. What changed and why.
2. Files touched and why.
3. Tests and checks run, with exact outcomes.
4. Known uncertainty or unverified behavior.
5. Risk and blast radius.
6. A practical rollback path.
