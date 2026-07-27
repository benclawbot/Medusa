# Medusa Release and Installation

## Supported release build

Medusa 1.0 targets Rust 1.88 or newer. A source installation requires Rust, Cargo, Git, and—when browser verification is used—Node.js 22 plus the pinned Playwright Chromium package.

```bash
cargo install --path crates/medusa-cli --locked
medusa doctor
```

`medusa doctor` checks Git, Cargo, Node.js, repository access, writable state, provider credentials, configured model, and schema compatibility. A missing `MINIMAX_API_KEY` is reported as a failed live-provider capability rather than silently ignored.

## Draft release publication

The permanent `Publish Draft Release` workflow runs only when an existing `v*` tag is pushed. The workflow event SHA, checked-out commit, and remote tag target must remain identical throughout the run. Before packaging, it requires:

- an exact `v<workspace-version>` tag;
- synchronized version metadata across the Rust workspace, desktop Cargo package, desktop `package.json`, and Tauri configuration;
- a tag commit contained in `main` history;
- passing deterministic release-evidence self-tests.

The workflow builds:

- Linux CLI archive, Debian package, and AppImage;
- macOS CLI archive, application archive, and DMG;
- Windows CLI archive and NSIS installer.

The final publish job downloads the independently built platform artifacts, rejects missing, duplicate, symlinked, or path-escaping assets, and produces:

- `medusa-release-manifest.json` with byte lengths and SHA-256 digests;
- `SHA256SUMS`;
- `medusa-sbom.cdx.json`, a deterministic CycloneDX 1.6 SBOM generated from `Cargo.lock` and the desktop `package-lock.json`;
- `LICENSE`, this release guide, and `COMPATIBILITY.md`.

`actions/attest@v4` generates GitHub/Sigstore provenance for every assembled asset with a short-lived OIDC identity. Only the final job receives `contents: write`, `id-token: write`, and `attestations: write`; build jobs remain read-only. The workflow creates a **draft** GitHub Release and refuses to overwrite an existing release. It never publishes automatically or pushes repository changes.

## Platform signing

The manually approved `Sign Draft Release` workflow operates only on an existing draft release whose tag resolves to `main`. It uses the protected `release-signing` environment and refuses incomplete signing output:

- Windows executable assets are Authenticode-signed, timestamped, and verified with `signtool`;
- the macOS application is Developer ID signed, notarized, stapled, and assessed with Gatekeeper;
- Linux package blobs receive keyless Sigstore signatures bound to the exact signing workflow identity.

Signing credentials remain encrypted environment secrets and are imported only into ephemeral runner stores. The workflow replaces assets on the draft release but never publishes it. See [Release signing](RELEASE-SIGNING.md) for secret names, environment protection, custody, verification, rotation, and revocation requirements.

## Trust boundary

Draft release provenance establishes the repository, workflow, commit, tag, and build identity associated with each asset. Platform signatures establish publisher identity and platform trust for the signed artifact. Checksums establish byte identity. Users should verify all three forms of evidence; none is a substitute for the others.

The Linux workflow signs distributed package blobs with Sigstore. Medusa does not currently claim operation of a signed APT, RPM, or other package repository.

A maintainer must review installation behavior, checksums, SBOM, attestations, compatibility notes, platform signatures, and platform warnings before deciding whether to publish a draft. Certificate issuance, custody, expiry monitoring, rotation, revocation, and incident response remain maintainer responsibilities.

See [Desktop distribution](DESKTOP-DISTRIBUTION.md), [Release compatibility](COMPATIBILITY.md), and [Release signing](RELEASE-SIGNING.md).

## Verification

Verify a downloaded asset before execution:

```bash
sha256sum --check SHA256SUMS --ignore-missing
gh attestation verify <asset> --repo benclawbot/Medusa
```

Then verify the platform signature using Authenticode, Gatekeeper/codesign, or the adjacent Linux Sigstore certificate and signature, as appropriate. Finally run:

```bash
medusa --version
medusa doctor
```

The manifest and checksums are evidence for the complete draft asset set. A successful checksum without matching provenance and platform-signature evidence is not sufficient release verification.

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

Reverting the publication or signing workflow prevents future automated drafts or signing runs but does not delete existing draft releases, signatures, or attestations. Existing evidence remains auditable and must be removed explicitly when invalidated.

## Live MiniMax canary

A live canary is intentionally credential-gated. CI executes it only when `MINIMAX_API_KEY` is configured; absence of the secret cannot be represented as a successful live canary. Deterministic provider fixtures remain mandatory on every pull request.
