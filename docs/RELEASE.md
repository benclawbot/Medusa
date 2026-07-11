# Medusa Release and Installation

## Supported release build

Medusa 1.0 targets Rust 1.88 or newer. A source installation requires Rust, Cargo, Git, and—when browser verification is used—Node.js 22 plus the pinned Playwright Chromium package.

```bash
cargo install --path crates/medusa-cli --locked
medusa doctor
```

`medusa doctor` checks Git, Cargo, Node.js, repository access, writable state, provider credentials, configured model, and schema compatibility. A missing `MINIMAX_API_KEY` is reported as a failed live-provider capability rather than silently ignored.

## Upgrade

Back up the repository and run:

```bash
medusa --repo /path/to/repository migrate
```

Every migration creates a backup and a checksummed receipt before mutation. Unsupported downgrades are refused rather than guessed.

## Rollback

1. Stop the Medusa daemon.
2. Restore the previous Medusa binary or package version.
3. Use the migration receipt's backup directory to restore `.medusa` state.
4. Verify the restored state digest and run `medusa doctor`.
5. Re-run the repository's targeted verification before resuming a session.

Release packages must contain the binary, `SHA256SUMS`, an SBOM, this rollback document, license, and compatibility notes.

## Live MiniMax canary

A live canary is intentionally credential-gated. CI executes it only when `MINIMAX_API_KEY` is configured; absence of the secret cannot be represented as a successful live canary. Deterministic provider fixtures remain mandatory on every pull request.
