# Release signing

Medusa separates unsigned draft assembly from platform signing. `Publish Draft Release` creates immutable, checksummed, SBOM-backed, attested draft assets. `Sign Draft Release` is a manually approved workflow in the protected `release-signing` environment that signs an existing draft and refuses to operate on a published release.

## Required repository configuration

Create a protected GitHub environment named `release-signing` with required reviewers. Restrict deployment branches to `main` and tags. Store signing credentials only as encrypted environment secrets.

### Windows

- `WINDOWS_SIGNING_CERTIFICATE_BASE64`: base64-encoded Authenticode PFX
- `WINDOWS_SIGNING_CERTIFICATE_PASSWORD`: PFX password

The workflow imports the certificate into the ephemeral runner user store, signs every Windows executable asset with SHA-256 and a trusted timestamp, and verifies the Authenticode chain with `signtool verify /pa /all`.

### macOS

- `APPLE_DEVELOPER_ID_CERTIFICATE_BASE64`: base64-encoded Developer ID Application P12
- `APPLE_DEVELOPER_ID_CERTIFICATE_PASSWORD`: P12 password
- `APPLE_NOTARY_APPLE_ID`: Apple account used by notarytool
- `APPLE_NOTARY_PASSWORD`: app-specific password
- `APPLE_TEAM_ID`: Apple Developer team identifier

The workflow creates an ephemeral keychain, signs the application with hardened runtime and timestamping, submits it to Apple notarization, staples the ticket, and verifies it with `codesign`, `stapler`, and Gatekeeper.

### Linux

Linux release assets receive keyless Sigstore signatures and certificates using GitHub Actions OIDC. The workflow verifies each blob against the exact `sign-draft-release.yml@refs/heads/main` workflow identity before upload. This signs the distributed package blobs; it does not claim that Medusa operates an APT, RPM, or other package repository.

## Running the workflow

1. Push a version tag and allow `Publish Draft Release` to create the draft and provenance evidence.
2. Review the draft manifest, checksums, SBOM, package smoke reports, and attestations.
3. Run **Sign Draft Release** with the exact existing tag.
4. Approve the `release-signing` environment deployment.
5. Review the replaced assets and signature evidence before manually publishing the draft.

The signing workflow verifies that the tag exists in `main`, that the release remains a draft before and after signing, and that all three platform outputs exist. It never publishes the release.

## Credential custody

Certificates and passwords must never be committed, printed, included in artifacts, or copied into release notes. Maintainers are responsible for certificate issuance, least-privilege access, expiry monitoring, renewal, revocation, and incident response. Rotating a certificate does not invalidate historical checksums or provenance, but compromised signatures and affected releases must be explicitly revoked or withdrawn.

## Trust boundary

A successful signing run provides platform-native signatures for Windows and macOS plus Sigstore blob signatures for Linux. GitHub/Sigstore build provenance remains separate evidence connecting assets to the repository workflow and commit. Users should verify checksums, provenance, and the platform signature; none of these alone proves all three properties.
