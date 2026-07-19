# Medusa Release Compatibility

## Version

This document applies to Medusa `1.0.0`. A release tag must be exactly `v1.0.0` and must point to a commit contained in the repository's `main` history. The root Cargo workspace, desktop Cargo package, desktop `package.json`, and Tauri configuration must contain the same semantic version before release packaging begins.

## Supported platforms

Release evidence is produced for:

| Platform | CLI | Desktop |
|---|---|---|
| Linux | compressed executable archive | Debian package and AppImage |
| macOS | compressed executable archive | application archive and DMG |
| Windows | ZIP executable archive | NSIS installer |

The packages are built by GitHub-hosted runners for the runner architecture used by the release workflow. Platform and architecture details remain visible in the attached provenance attestation.

## Runtime requirements

- Git is required for repository operations.
- Provider-backed execution requires `MINIMAX_API_KEY` in the user's environment.
- Browser verification and Desktop Commander require Node.js 22 and their pinned packages.
- Linux desktop packages require the WebKitGTK runtime expected by the generated Tauri package.

## Repository state

Medusa stores repository-local state under `.medusa`. Release `1.0.0` supports the schema version declared by `medusa-hardening`. Run `medusa doctor` before an upgrade and `medusa --repo /path/to/repository migrate` when a migration is required.

Persisted daemon jobs retain the rollback-readable states `queued`, `running`, `succeeded`, `failed`, and `interrupted`. Newer binaries refuse unsupported downgrades rather than guessing at state conversion.

## Frontend compatibility

The TUI and desktop application are adapters over the same `medusa-runtime` controller and repository-scoped daemon protocol. A release must ship all CLI and desktop assets from the same commit and synchronized version; mixing frontend packages from different releases is unsupported.

## Trust boundary

GitHub provenance attestations and SHA-256 manifests establish which workflow, repository, commit, and tag produced an asset. They do not replace platform code signing:

- Windows installers are not Authenticode-signed.
- macOS applications are not Developer ID signed or notarized.
- Linux packages are not published through a signed distribution repository.

A release remains a draft until a maintainer reviews the assets, checksums, SBOM, attestations, installation results, and these limitations. Platform signing and notarization require separate certificate-custody procedures.

## Verification

After downloading an asset:

1. compare its digest with `SHA256SUMS`;
2. inspect `medusa-release-manifest.json` for the expected path and byte length;
3. verify provenance with `gh attestation verify <asset> --repo benclawbot/Medusa`;
4. run `medusa --version` and `medusa doctor` before using the release on an existing repository.

## Rollback

Stop the repository daemon, restore the prior binary or desktop package, restore `.medusa` from the migration receipt if a schema change occurred, and run `medusa doctor`. See `RELEASE.md` for the complete rollback sequence.
