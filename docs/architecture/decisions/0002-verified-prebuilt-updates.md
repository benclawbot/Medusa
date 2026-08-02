# ADR 0002: Verified prebuilt releases are the default update authority

- Status: accepted
- Date: 2026-08-02
- Issue: #655
- Owners: release and security maintainers

## Context

The legacy `medusa update` path resolved the moving `main` branch and invoked Cargo to compile and install a replacement. That path was slow, depended on a local Rust toolchain and source network, could not bind the installed bytes to a reviewed release, and provided no signed platform manifest or health-checked rollback contract.

The existing release workflow already produced free GitHub-hosted CLI and desktop artifacts for Linux, macOS, and Windows, plus checksums and attestations. The missing architecture was a client-verifiable release authority and a safe installation state machine.

## Decision

The stable release channel is the default and automatic updater authority.

A protected release-signing workflow creates a canonical `medusa-release-manifest-v2` from CI-produced release assets and signs the exact bytes with Ed25519. The updater embeds reviewed public keys, verifies the signature before parsing or trusting manifest fields, selects one exact OS/architecture CLI artifact, streams and verifies the signed byte count and SHA-256 digest, confines extraction, stages adjacent to the running executable, and performs a health-checked atomic replacement with automatic rollback.

A source-build updater remains available only through the explicit developer command `medusa update --channel source`. It is not a fallback and cannot be selected by the stable or automatic release path.

## Consequences

- End users no longer need Rust or Cargo for normal updates.
- GitHub Releases remains the free artifact transport; trust does not depend on mutable release prose or unsigned asset metadata.
- A release is not update-eligible until its signed manifest assets are present.
- Release signing requires the protected `release-signing` environment and the `MEDUSA_RELEASE_ED25519_PRIVATE_KEY_PEM` secret.
- The repository public-key keyring and embedded updater trust store must change before a signing-key rotation becomes active.
- Downgrades and rollout-sequence rollback require explicit local approval.
- Package-managed installations are never silently replaced or upgraded through a package manager.
- Failures preserve the current executable, retain diagnostics, and do not silently compile source.

## Rejected alternatives

### Continue compiling `main`

Rejected because it is not release-bound, is slow, depends on a local toolchain, and expands the network and supply-chain surface.

### Trust only SHA256SUMS

Rejected because an unsigned checksum file can be replaced together with an artifact.

### Trust only GitHub asset metadata or release attestations

Rejected as the runtime trust root because the updater needs a stable offline-verifiable key, explicit platform mapping, rollout sequence, minimum updater version, and revocation/rotation policy. Attestations remain complementary release evidence.

### Invoke Homebrew, apt, or another package manager automatically

Rejected because package-manager authority, elevation, mirrors, and rollback behavior differ by installation. The updater reports the appropriate command and leaves execution to the operator.

### Replace immediately without startup acknowledgement

Rejected because a byte-valid binary can still fail to start on a specific host. The previous executable remains recoverable until the replacement acknowledges startup.

## Evidence

- `crates/medusa-update/src/manifest.rs`
- `crates/medusa-update/src/github.rs`
- `crates/medusa-update/src/install.rs`
- `crates/medusa-cli/src/update_command.rs`
- `release/keys/keyring.json`
- `.github/workflows/verified-prebuilt-update.yml`
- `.github/workflows/sign-release-manifest.yml`
- `scripts/release-evidence.py`
- `docs/architecture/PREBUILT-UPDATES.md`
