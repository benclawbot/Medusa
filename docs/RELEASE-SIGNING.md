# Release signing

Medusa separates build provenance, platform publisher signing, and updater release authority.

- `Publish Release` assembles and attests CI-produced artifacts.
- `Sign Draft Release` applies platform-native signatures to an existing draft.
- `Sign Release Manifest` creates the Ed25519 authority consumed by `medusa update --release` after a release is published.

All signing jobs use the protected `release-signing` environment.

## Required repository configuration

Create a GitHub environment named `release-signing` with required reviewers. Restrict deployment branches to `main` and release tags. Store credentials only as encrypted environment secrets. No signing key or password may be stored in repository content, build artifacts, logs, release notes, or caches.

### Updater Ed25519 authority

- `MEDUSA_RELEASE_PRIMARY_ED25519_PRIVATE_KEY_PEM`: PEM-encoded private Ed25519 key matching the active `primary` entry in `release/keys/keyring.json`.
- `MEDUSA_RELEASE_RECOVERY_ED25519_PRIVATE_KEY_PEM`: independently generated PEM-encoded private Ed25519 key matching the active `recovery` entry.

`Sign Release Manifest` downloads the existing CI-produced assets from a stable release, regenerates the canonical `medusa-release-manifest-v2`, signs the exact bytes with the primary authority by default, verifies the result against the matching repository public key, attests the manifest, and uploads the manifest, signature envelope, and checksum inventory. A protected manual dispatch may select the recovery authority without editing the workflow or replacing the primary secret.

The private key is written only to a mode-0600 temporary runner file and removed through a shell trap. The workflow fails when the secret is absent, the tag does not match synchronized repository versions, the release assets are incomplete, any asset basename is duplicated, an asset escapes the download directory, signing fails, or local verification fails.

A published release is not update-eligible until this workflow succeeds. `medusa update --release` rejects an unsigned release rather than using GitHub metadata or source compilation as a fallback. The default `medusa update` main-branch path is separate and does not consult the release manifest.

### Windows platform signing

- `WINDOWS_SIGNING_CERTIFICATE_BASE64`: base64-encoded Authenticode PFX
- `WINDOWS_SIGNING_CERTIFICATE_PASSWORD`: PFX password

The workflow imports the certificate into the ephemeral runner user store, signs Windows executable assets with SHA-256 and a trusted timestamp, and verifies the Authenticode chain with `signtool verify /pa /all`.

### macOS platform signing

- `APPLE_DEVELOPER_ID_CERTIFICATE_BASE64`: base64-encoded Developer ID Application P12
- `APPLE_DEVELOPER_ID_CERTIFICATE_PASSWORD`: P12 password
- `APPLE_NOTARY_APPLE_ID`: Apple account used by notarytool
- `APPLE_NOTARY_PASSWORD`: app-specific password
- `APPLE_TEAM_ID`: Apple Developer team identifier

The workflow creates an ephemeral keychain, signs the application with hardened runtime and timestamping, submits it for notarization, staples the ticket, and verifies it with `codesign`, `stapler`, and Gatekeeper.

### Linux platform signing

Linux package assets receive keyless Sigstore signatures and certificates through GitHub Actions OIDC. The workflow verifies each blob against the exact signing workflow identity before upload. This signs distributed package blobs; it does not claim that Medusa operates an APT, RPM, or other package repository.

## Release procedure

1. Push the exact version tag and allow the release build workflow to create the cross-platform artifacts, SBOM, checksums, and attestations.
2. Review package smoke reports and build provenance.
3. Run **Sign Draft Release** when platform-native signatures are required, approve the protected environment, and verify all platform outputs.
4. Publish the stable release.
5. Approve the automatically triggered **Sign Release Manifest** deployment.
6. Verify that the release contains `medusa-release-manifest.json`, `medusa-release-manifest.sig.json`, and `SHA256SUMS` before announcing `medusa update --release` availability.

A signing rerun may use `workflow_dispatch` with an existing stable tag. It regenerates the authority from the release's current CI artifacts and replaces only the three manifest-authority assets.

## Key custody, rotation, and revocation

The machine-readable public-key lifecycle is `release/keys/keyring.json`. `scripts/check-release-keyring.py` requires distinct active primary and recovery roles, unique GitHub secret references, matching Ed25519 PEM/hex material, valid sequence windows, and at least the declared overlap. `medusa-update` tests compare its embedded trust store with the same keyring.

The first published `v1.0.0` release did not contain a signature envelope, so the original `medusa-release-2026-01` public key never authorized an update-eligible release. It is retained as a revoked audit record. The independently generated `medusa-release-2026-08-primary` and `medusa-release-2026-08-recovery` authorities begin at sequence 1 and are both embedded before the first signed release.

Before rotation:

1. generate the replacement Ed25519 key outside the repository;
2. add its public key, unique key ID, active status, and sequence window to the keyring and updater trust store;
3. release an updater that trusts both keys, signed by an authority already trusted by the installed population;
4. retain at least two stable-release overlaps;
5. switch the default role only after the overlap; keep the separately named recovery secret available for reviewed manual dispatch;
6. bound the previous key's final sequence after the overlap.

A revoked key remains in the keyring with `status: revoked`; the updater rejects it even when a signature is mathematically valid. Compromise response must rotate the protected secret, revoke the public key in code, withdraw affected releases or manifests, publish a security notice, and ship an updater trust-store release through a still-trusted key. Historical checksums and attestations remain evidence but do not make a revoked release eligible.

Primary and recovery private keys must be generated independently on a controlled maintainer system or hardware-backed signer, stored under separate GitHub environment secret names, and backed up outside the repository with user-only access and approved encryption/custody. Recovery drills sign and verify a disposable manifest with each authority; they do not upload or expose private material. Rotate before operational expiry or immediately after suspected exposure.

## Trust boundary

Ed25519 manifest verification proves that Medusa release maintainers authorized exact artifact metadata for the release updater. GitHub attestations prove workflow provenance. Authenticode, Developer ID, notarization, and Linux Sigstore evidence prove publisher or platform identity. SHA-256 proves byte identity. These controls are complementary and none replaces the others.

See [Verified prebuilt update architecture](architecture/PREBUILT-UPDATES.md) and [ADR 0002](architecture/decisions/0002-verified-prebuilt-updates.md).
