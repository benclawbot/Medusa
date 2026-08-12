# Medusa Desktop Distribution

## Package layers

Medusa Desktop has two distribution layers:

1. the read-only `Desktop` workflow builds unsigned packages for pull-request and `main` validation and retains them as short-lived GitHub Actions artifacts;
2. the tag-only `Publish Draft Release` workflow rebuilds the same package classes, combines them with CLI assets and release evidence, creates provenance attestations, and opens a draft GitHub Release.

Neither layer performs Windows Authenticode signing, macOS Developer ID signing or notarization, or Linux repository signing.

## Validated targets

| Platform | Bundles |
|---|---|
| Linux | Debian package (`.deb`) and AppImage (`.AppImage`) |
| macOS | application archive (`.zip`) and disk image (`.dmg`) |
| Windows | NSIS installer (`.exe`) |

The `Desktop` workflow also validates the uncompressed macOS `.app` tree before it is archived for release.

## Shared runtime and durable sessions

The desktop application is a thin Tauri adapter over `medusa-runtime`; it does not maintain a separate agent implementation. It uses the same controller, provider configuration, skills, tools, policy, cancellation, follow-up queue, plans, questions, memory, and repository-scoped daemon supervisor as the TUI.

The **Sessions** dock lists durable sessions for the active repository and can preview their stored transcript. Selecting **Resume this session** starts the desktop through `RuntimeController::start_resumed` and preserves the session identity, objective, turn state, plan, pending question, transcript, and evidence chain.

The resume request is one-shot and is cleared during desktop startup. Invalid identifiers, missing sessions, repository mismatches, corrupted state, and failed integrity checks return an error instead of silently opening a blank replacement conversation. See [Durable session resume](SESSION-RESUME.md) for the complete behavior and validation evidence.

## Continuous package validation

The read-only desktop bundle job:

1. installs the pinned Node.js and Rust toolchains;
2. installs platform build prerequisites;
3. runs `npm ci` from `apps/medusa-desktop`;
4. builds only the supported target formats;
5. rejects missing, duplicate, unexpectedly small, or path-escaping artifacts;
6. hashes the resulting files and macOS application tree;
7. uploads the bundle directory and `desktop-package-<platform>.json` for 14 days.

The package validator includes deterministic fixtures proving that valid packages pass while missing, tiny, duplicate, and escaping artifacts fail.

Version metadata is checked independently across:

- the root Cargo workspace version;
- `apps/medusa-desktop/src-tauri/Cargo.toml`;
- `apps/medusa-desktop/package.json`;
- `apps/medusa-desktop/src-tauri/tauri.conf.json`.

A mismatch or invalid semantic version fails before packaging.

## Draft release assembly

The release workflow accepts only an existing tag that exactly matches `v<version>` and points into `main` history. Linux, macOS, and Windows jobs build independently with read-only repository permissions. Their normalized release assets are downloaded by one final job.

Before creating a draft, the final job requires exactly one of every expected CLI and desktop asset, rejects symlinks and duplicate basenames, and generates:

- a complete JSON asset manifest;
- `SHA256SUMS`;
- a deterministic CycloneDX SBOM from the locked Rust and npm graphs;
- compatibility, release, rollback, and license documents;
- GitHub/Sigstore provenance attestations for every assembled asset.

Only that final job receives `contents: write`, `id-token: write`, and `attestations: write`. The permission is registered in `docs/workflow-write-allowlist.txt`. The workflow creates a draft release, refuses to overwrite an existing release, and cannot publish automatically.

## Obtaining a CI package

1. Open the repository's **Actions** page.
2. Select the successful **Desktop** workflow run for the desired commit.
3. Download the artifact named `medusa-desktop-<platform>-<commit-sha>`.
4. Verify that the package digest matches the bundled JSON manifest before testing it.

CI artifacts are intended for development, smoke testing, and installation-path verification.

## Verifying a draft release asset

Download the draft asset together with `SHA256SUMS`, then verify both its digest and provenance:

```bash
sha256sum --check SHA256SUMS --ignore-missing
gh attestation verify <asset> --repo benclawbot/Medusa
```

The attestation identifies the workflow, repository, commit, tag, and short-lived signing identity that produced the asset. It is not a substitute for operating-system code signing.

## Local package build

From the repository root, first validate the version metadata and release evidence implementation:

```bash
python scripts/check-desktop-version-sync.py --root . --self-test
python scripts/release-evidence.py self-test
```

Then install dependencies and build the platform's supported targets:

```bash
cd apps/medusa-desktop
npm ci
```

Linux:

```bash
APPIMAGE_EXTRACT_AND_RUN=1 npm run tauri:build -- --bundles deb,appimage
```

macOS:

```bash
npm run tauri:build -- --bundles app,dmg
```

Windows PowerShell:

```powershell
npm run tauri:build -- --bundles nsis
```

Validate the result from the repository root:

```bash
python scripts/desktop-package-smoke.py \
  --root apps/medusa-desktop/src-tauri/target/release/bundle \
  --platform <linux|macos|windows> \
  --manifest desktop-package.json
```

## Unsigned-package limitations

These packages are not yet trusted end-user releases:

- Windows SmartScreen may warn because the NSIS installer is not Authenticode-signed.
- macOS Gatekeeper may block or warn because the app is not Developer ID signed or notarized.
- Linux packages are not published through a signed repository and carry no distribution-maintainer signature.
- SHA-256 evidence and GitHub provenance establish build origin and integrity, but do not grant platform trust.

Signing, notarization, certificate custody, rotation, revocation, and public release approval require a separate design. Long-lived signing credentials must never be exposed to pull-request workflows.

## Rollback

Reverting the desktop or publication workflows restores the prior CI behavior and prevents future automated drafts. Existing workflow artifacts expire according to retention. Existing draft releases and attestations remain auditable and require explicit deletion if invalidated.
