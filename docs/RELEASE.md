# Medusa Release and Installation

## Supported release build

Medusa 1.0 targets Rust 1.88 or newer. A source installation requires Rust, Cargo, Git, and—when browser verification is used—Node.js 22 plus the pinned Playwright Chromium package.

```bash
cargo install --path crates/medusa-cli --locked
medusa doctor
```

`medusa doctor` checks Git, Cargo, Node.js, repository access, writable state, provider credentials, configured model, and schema compatibility. A missing `MINIMAX_API_KEY` is reported as a failed live-provider capability rather than silently ignored.

## Desktop package evidence

The `Desktop` workflow builds unsigned application packages as read-only workflow artifacts:

- Linux: Debian package and AppImage;
- macOS: application bundle and DMG;
- Windows: NSIS installer.

The workflow verifies synchronized `1.0.0` release metadata across the Rust workspace, desktop Cargo package, `package.json`, and Tauri configuration. It rejects missing, duplicate, unexpectedly small, or path-escaping packages and emits a JSON manifest containing relative paths, byte lengths, and SHA-256 digests.

These CI artifacts are suitable for installation and packaging smoke tests, not trusted public distribution. Windows signing, macOS Developer ID signing and notarization, Linux repository signing, certificate custody, and publication are intentionally separate release work. See [Desktop distribution](DESKTOP-DISTRIBUTION.md).

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

Release packages must contain the binary, `SHA256SUMS`, an SBOM, this rollback document, license, and compatibility notes. Desktop workflow artifacts currently provide package-specific SHA-256 manifests but are not yet published release packages and therefore do not satisfy the complete signed-release contract.

## Live MiniMax canary

A live canary is intentionally credential-gated. CI executes it only when `MINIMAX_API_KEY` is configured; absence of the secret cannot be represented as a successful live canary. Deterministic provider fixtures remain mandatory on every pull request.
