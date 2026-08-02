# Medusa Release and Installation

## Supported installation

Normal installation and updates use prebuilt release artifacts; Rust and Cargo are not required. Source installation remains available for contributors:

```bash
cargo install --path crates/medusa-cli --locked
medusa doctor
```

Medusa 1.0 source builds require Rust 1.88 or newer. Desktop and browser development additionally require Node.js 22 and the pinned browser package.

## Release artifacts

`Publish Release` validates an exact `v<workspace-version>` tag bound to the workflow SHA and contained in `main`, then independently builds:

- Linux CLI archive, Debian package, and AppImage;
- macOS CLI archive, application archive, and DMG;
- Windows CLI archive and NSIS installer;
- deterministic CycloneDX SBOM, checksums, compatibility notes, license, and release guide.

The build workflow rejects missing, duplicate, symlinked, or path-escaping assets and attests the CI-produced bytes with GitHub/Sigstore provenance.

A published release is not update-eligible until `Sign Release Manifest` completes. That protected workflow downloads the existing CI-produced release assets, regenerates a canonical `medusa-release-manifest-v2`, signs the exact manifest bytes with Ed25519, verifies the signature against the repository public key, attests the manifest authority, and uploads:

- `medusa-release-manifest.json`;
- `medusa-release-manifest.sig.json`;
- `SHA256SUMS`.

The updater fails closed while those assets are absent or invalid.

## Manifest trust

The signed manifest binds:

- semantic version and minimum updater version;
- source repository and exact revision;
- Rust toolchain and lockfile digests;
- stable rollout sequence and percentage;
- exact artifact name, kind, operating system, architecture, target triple, byte count, and SHA-256 digest.

The updater verifies the Ed25519 signature before parsing or trusting those fields. GitHub release metadata supplies only the fixed manifest and signature bootstrap URLs. The reviewed keyring is stored in `release/keys/keyring.json`; private signing material is never committed.

## Update

Check without modifying the installation:

```bash
medusa update --check
```

Apply the latest eligible stable release:

```bash
medusa update
```

For unattended managed execution, approval must be explicit:

```bash
medusa update --automatic
```

The running session remains usable while the updater checks, downloads, verifies, and stages the release. The updater then requests daemon shutdown, exits, atomically replaces the binary, restarts with the same repository and `--continue`, and requires a startup health acknowledgement. The previous executable is retained until acknowledgement and is restored automatically on swap failure, timeout, or early exit.

Package-managed installations are not silently replaced. Medusa reports the appropriate package-manager command and leaves execution to the operator.

### Explicit source developer channel

Source compilation is no longer the default and is never a fallback. Contributors may deliberately select it:

```bash
medusa update --channel source
```

That command warns that it invokes Cargo and follows the moving main branch.

### Downgrades and rollout rollback

A semantic-version downgrade or a release with a lower rollout sequence is rejected unless the operator explicitly passes:

```bash
medusa update --allow-downgrade
```

This flag does not bypass signature, platform, archive, size, or digest verification.

## Failure behavior

The current binary remains usable when release discovery, signature verification, platform selection, download, size or digest verification, extraction, staging, or restart fails. Partial downloads may resume, but they never exceed the signed byte count and are never promoted before full verification. Source compilation is not attempted after a release-channel failure.

Path-free phase diagnostics are appended to `.medusa/update-diagnostics.jsonl`. Replacement state is written beside the executable so interrupted swaps and automatic rollback remain observable.

## Platform signatures

`Sign Draft Release` provides platform publisher signatures independently of the updater manifest:

- Windows Authenticode signing and timestamp verification;
- macOS Developer ID signing, notarization, stapling, and Gatekeeper assessment;
- Linux keyless Sigstore signatures for distributed package blobs.

The Ed25519 manifest proves the updater's release authority and exact artifact metadata. Platform signatures prove publisher/platform identity. GitHub attestations prove workflow provenance. SHA-256 proves byte identity. These controls are complementary.

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
