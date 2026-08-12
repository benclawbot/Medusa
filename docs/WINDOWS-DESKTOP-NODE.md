# Windows desktop Node.js and npm dependency

Medusa Desktop's **Update from main** flow builds the React/Tauri application locally. On Windows this requires Node.js 22, npm, Git, and Cargo.

## Installer behavior

The Windows NSIS installer checks for both `node.exe` and `npm.cmd` after installing Medusa Desktop. When either command is missing, the installer runs WinGet non-interactively to install Node.js 22.21.1 from the `OpenJS.NodeJS` package. npm is included by the Node.js installer.

The installation stops with an actionable error when WinGet is unavailable, the package installation fails, or `npm.cmd` is still absent. This prevents a successful-looking Medusa installation whose source updater cannot run.

## Updater discovery behavior

Windows exposes npm as `npm.cmd`, not as a native `npm.exe`. The desktop updater therefore checks:

- `npm.cmd` through the current process search path;
- `%ProgramFiles%\nodejs\npm.cmd` and `%ProgramW6432%\nodejs\npm.cmd`;
- `%LOCALAPPDATA%\Programs\nodejs\npm.cmd` for per-user installations.

Before the detached update helper builds Medusa, it rebuilds its `PATH` from the current machine and user registry environment values. It then resolves `npm.cmd` again and falls back to the standard Node.js installation directories. A Node installation performed by the NSIS installer is therefore usable immediately; reopening Windows, signing out, or restarting Medusa solely to refresh `PATH` is not required.

## Verification

The Rust regression tests verify that the Windows update helper refreshes `PATH`, resolves `npm.cmd`, uses the resolved command for both `npm ci` and `npm run build`, and retains the existing replace-and-restart behavior. A Windows-only regression test verifies that dependency detection includes both the `npm.cmd` command name and standard Node.js installation locations.

For a local Windows validation run:

```powershell
cargo test --manifest-path apps/medusa-desktop/src-tauri/Cargo.toml desktop_update
cd apps/medusa-desktop
npm ci
npm run typecheck
npm test
npm run tauri:build -- --bundles nsis
```
