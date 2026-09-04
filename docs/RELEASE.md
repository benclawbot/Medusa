# Medusa Release and Installation

## Supported installation

Normal installation uses prebuilt release artifacts; Rust and Cargo are not required to install a stable Medusa build. Update behavior has two explicit paths:

- `medusa update` follows the latest `main` commit through an immutable commit-scoped rolling prebuilt, so it waits for that exact platform artifact to be published;
- `medusa update --release` installs the latest eligible verified prebuilt release and does not require Rust or Cargo.

Source installation remains available for contributors:

```bash
cargo install --path crates/medusa-cli --locked
medusa doctor
```

Medusa 1.0 source builds require Rust 1.88 or newer. Desktop and browser development additionally require Node.js 22 and the pinned browser package.

## Release artifacts

`Publish Release` validates an exact `v<release-version>` tag bound to the workflow SHA and contained in `main`, then independently builds:

- Linux CLI archive, Debian package, and AppImage;
- macOS CLI archive, application archive, and DMG;
- Windows CLI archive and NSIS installer;
- deterministic CycloneDX SBOM, checksums, compatibility notes, license, and release guide.

The build workflow rejects missing, duplicate, symlinked, or path-escaping assets, attests the CI-produced bytes with GitHub/Sigstore provenance, and creates the GitHub release as a draft. Stable release assets are not rewritten after publication.

`Sign Release Manifest` is the publication boundary. The protected workflow operates only on the still-draft release, downloads the complete CI-produced assets, regenerates a canonical `medusa-release-manifest-v2`, signs the exact manifest bytes with Ed25519, verifies the signature against the repository public key, attests the manifest authority, and uploads:

- `medusa-release-manifest.json`;
- `medusa-release-manifest.sig.json`;
- `SHA256SUMS`.

Only after those authority files are attached and verified does the workflow publish the draft. A post-publish matrix then downloads the public Linux, macOS, and Windows CLI archives and runs the released `medusa update --check --release` against the public GitHub API. A failed public verification is a failed release workflow, not an invitation to mutate the published assets.

## Manifest trust

The signed manifest binds:

- package semantic version, four-part release identity, and minimum updater version;
- source repository and exact revision;
- Rust toolchain and lockfile digests;
- stable rollout sequence and percentage;
- exact artifact name, kind, operating system, architecture, target triple, byte count, and SHA-256 digest.

The release updater verifies the Ed25519 signature before parsing or trusting those fields. GitHub release metadata supplies only the fixed manifest and signature bootstrap URLs. The reviewed primary/recovery lifecycle is stored in `release/keys/keyring.json`; private signing material is never committed, and CI rejects a keyring that cannot satisfy its overlap policy.

## Update

### Latest main

Check the latest `main` commit without modifying the installation:

```bash
medusa update --check
```

Build and install the latest `main` content:

```bash
medusa update
```

This path resolves the moving `main` branch and downloads only the immutable rolling release tagged for that exact commit. The rolling release is published only after all platform builds and manifest/hash validation succeed; it never falls back to a stable release if the exact rolling artifact is unavailable.

For unattended managed execution, approval must be explicit:

```bash
medusa update --automatic
```

### Stable verified release

Check the latest signed stable release without modifying the installation:

```bash
medusa update --release --check
```

Install the latest eligible signed stable release:

```bash
medusa update --release
```

For unattended managed release updates:

```bash
medusa update --release --automatic
```

The release path requires `medusa-release-manifest.json` and `medusa-release-manifest.sig.json`, verifies the Ed25519 authority and artifact metadata, and never falls back to `main` when verification fails.

The running session remains usable while the release updater checks, downloads, verifies, and stages the release. The updater then requests daemon shutdown, exits, atomically replaces the binary, restarts with the same repository and `--continue`, and requires a startup health acknowledgement. The previous executable is retained until acknowledgement and is restored automatically on swap failure, timeout, or early exit.

Package-managed installations are not silently replaced. Medusa reports the appropriate package-manager command and leaves execution to the operator.

### Downgrades and rollout rollback

A semantic-version downgrade or a release with a lower rollout sequence is rejected unless the operator explicitly selects the release path and passes:

```bash
medusa update --release --allow-downgrade
```

`--allow-downgrade` is release-only. It does not bypass signature, platform, archive, size, or digest verification.

## Failure behavior

The current binary remains usable when update discovery, release signature verification, platform selection, download, size or digest verification, extraction, staging, compilation, or restart fails. Partial release downloads may resume, but they never exceed the signed byte count and are never promoted before full verification. Release verification failures never trigger source compilation, and source-path failures never trigger release installation.

Path-free release phase diagnostics are appended to `.medusa/update-diagnostics.jsonl`. Replacement state is written beside the executable so interrupted swaps and automatic rollback remain observable.

## Platform signatures

`Sign Draft Release` provides platform publisher signatures independently of the updater manifest and only while a release remains a draft:

- Windows Authenticode signing and timestamp verification;
- macOS Developer ID signing, notarization, stapling, and Gatekeeper assessment;
- Linux keyless Sigstore signatures for distributed package blobs.

When platform-native signing is required, complete it before approving or rerunning the manifest-authority signer so the Ed25519 manifest binds the final signed bytes. The Ed25519 manifest proves the updater's release authority and exact artifact metadata. Platform signatures prove publisher/platform identity. GitHub attestations prove workflow provenance. SHA-256 proves byte identity. These controls are complementary.

See [Release signing](RELEASE-SIGNING.md), [Desktop distribution](DESKTOP-DISTRIBUTION.md), [Release compatibility](COMPATIBILITY.md), and [Verified prebuilt update architecture](architecture/PREBUILT-UPDATES.md).

## Manual verification

```bash
sha256sum --check SHA256SUMS --ignore-missing
gh attestation verify <asset> --repo benclawbot/Medusa
medusa --version
medusa doctor
```

Also verify Authenticode, Gatekeeper/codesign, or the adjacent Linux Sigstore evidence as appropriate.

## State migration and rollback

Repository-state migration remains separate from binary replacement:

```bash
medusa --repo /path/to/repository migrate
```

Each migration creates a backup and checksummed receipt before mutation. To roll back repository state, stop the daemon, restore the prior package or binary, restore the receipt backup, verify its digest, run `medusa doctor`, and run targeted repository verification before resuming.

## Live MiniMax canary

The live provider canary runs only when `MINIMAX_API_KEY` is configured. Missing credentials cannot be represented as a successful live canary; deterministic provider fixtures remain mandatory on every pull request.
