# Medusa Desktop Distribution

## Current package boundary

Medusa Desktop is built as an unsigned Tauri application on Linux, macOS, and Windows. The `Desktop` workflow creates packages as read-only GitHub Actions artifacts; it does not publish a GitHub Release, modify repository contents, or access signing credentials.

The validated targets are:

| Platform | Bundles |
|---|---|
| Linux | Debian package (`.deb`) and AppImage (`.AppImage`) |
| macOS | application bundle (`.app`) and disk image (`.dmg`) |
| Windows | NSIS installer (`.exe`) |

Each workflow artifact also contains `desktop-package-<platform>.json`. The manifest records each expected package's relative path, byte length, and SHA-256 digest.

## CI validation

The desktop bundle job:

1. installs the pinned Node.js and Rust toolchains;
2. installs platform build prerequisites;
3. runs `npm ci` from `apps/medusa-desktop`;
4. builds only the target formats listed above;
5. rejects missing, duplicate, unexpectedly small, or path-escaping artifacts;
6. hashes the resulting files and macOS application tree;
7. uploads the bundle directory and evidence manifest for 14 days.

The package validator includes deterministic fixtures proving that valid packages pass while missing, tiny, and escaping artifacts fail.

Version metadata is checked independently across:

- the root Cargo workspace version;
- `apps/medusa-desktop/src-tauri/Cargo.toml`;
- `apps/medusa-desktop/package.json`;
- `apps/medusa-desktop/src-tauri/tauri.conf.json`.

A mismatch or non-semantic version fails before packaging.

## Obtaining a CI package

1. Open the repository's **Actions** page.
2. Select the successful **Desktop** workflow run for the desired commit.
3. Download the artifact named `medusa-desktop-<platform>-<commit-sha>`.
4. Verify that the package digest matches the bundled JSON manifest before testing it.

The artifacts are intended for development, smoke testing, and installation-path verification.

## Local package build

From the repository root, first validate the version metadata:

```bash
python scripts/check-desktop-version-sync.py --root . --self-test
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

These artifacts are not trusted end-user releases:

- Windows SmartScreen may warn because the NSIS installer is not Authenticode-signed.
- macOS Gatekeeper may block or warn because the app is not Developer ID signed or notarized.
- Linux packages are not published through a signed repository and carry no distribution-maintainer signature.
- GitHub Actions artifact integrity and the included SHA-256 evidence do not replace platform code signing.

Signing, notarization, certificate custody, provenance attestations, and publication require a separate design. That work must preserve least privilege, avoid exposing long-lived credentials to pull requests, and define certificate rotation and rollback before packages can be described as production-trusted releases.

## Rollback

This packaging layer changes no runtime state or repository schema. Reverting the desktop workflow and validation scripts restores test-only desktop CI. Existing unsigned artifacts expire according to their workflow retention period and are not automatically installed or published.
