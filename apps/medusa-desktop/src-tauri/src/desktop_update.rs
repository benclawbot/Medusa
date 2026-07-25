use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
};

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use medusa_update::MainBranchUpdater;
use serde::Serialize;

const REPOSITORY_URL: &str = "https://github.com/benclawbot/Medusa.git";
const BRANCH: &str = "main";

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopUpdateStatus {
    current_version: String,
    latest_main_sha: String,
    executable: String,
    ready: bool,
    missing_dependencies: Vec<String>,
}

#[tauri::command]
pub fn desktop_update_status() -> Result<DesktopUpdateStatus, String> {
    status().map_err(|error| error.to_string())
}

#[tauri::command]
pub fn desktop_update_from_main(app: tauri::AppHandle) -> Result<(), String> {
    schedule_update(&app).map_err(|error| error.to_string())?;
    app.exit(0);
    Ok(())
}

fn status() -> MedusaResult<DesktopUpdateStatus> {
    let executable = env::current_exe()?;
    let latest_main_sha = MainBranchUpdater::public()?.latest_main()?.sha;
    let missing_dependencies = missing_dependencies();
    Ok(DesktopUpdateStatus {
        current_version: env!("CARGO_PKG_VERSION").to_owned(),
        latest_main_sha,
        executable: executable.display().to_string(),
        ready: missing_dependencies.is_empty(),
        missing_dependencies,
    })
}

fn schedule_update(app: &tauri::AppHandle) -> MedusaResult<()> {
    let missing = missing_dependencies();
    if !missing.is_empty() {
        return Err(MedusaError::new(
            ErrorCode::DependencyUnavailable,
            ErrorCategory::Environment,
            format!("updating from main requires: {}", missing.join(", ")),
        ));
    }

    let executable = env::current_exe()?;
    let parent_pid = std::process::id();
    let helper = update_helper_path()?;

    #[cfg(windows)]
    {
        fs::write(&helper, windows_update_script(parent_pid, &executable))?;
        Command::new("powershell")
            .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
            .arg(&helper)
            .spawn()
            .map_err(command_error)?;
    }

    #[cfg(not(windows))]
    {
        fs::write(&helper, unix_update_script(parent_pid, &executable))?;
        Command::new("sh")
            .arg(&helper)
            .spawn()
            .map_err(command_error)?;
    }

    let _ = app;
    Ok(())
}

fn missing_dependencies() -> Vec<String> {
    ["git", "npm", "cargo"]
        .into_iter()
        .filter(|program| !command_available(program))
        .map(str::to_owned)
        .collect()
}

fn command_available(program: &str) -> bool {
    Command::new(program)
        .arg("--version")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

fn update_helper_path() -> MedusaResult<PathBuf> {
    let extension = if cfg!(windows) { "ps1" } else { "sh" };
    let path = env::temp_dir().join(format!(
        "medusa-desktop-update-{}-{BRANCH}.{extension}",
        std::process::id()
    ));
    Ok(path)
}

#[cfg(not(windows))]
fn unix_update_script(parent_pid: u32, executable: &Path) -> String {
    let executable = shell_quote(executable);
    format!(
        r#"#!/bin/sh
set -eu
work="${{TMPDIR:-/tmp}}/medusa-desktop-main-{parent_pid}"
rm -rf "$work"
git clone --depth 1 --branch {BRANCH} '{REPOSITORY_URL}' "$work"
cd "$work/apps/medusa-desktop"
npm ci
npm run build
cargo build --release --manifest-path src-tauri/Cargo.toml
built="$work/apps/medusa-desktop/src-tauri/target/release/medusa-desktop"
while kill -0 {parent_pid} 2>/dev/null; do sleep 1; done
cp "$built" {executable}.new
chmod +x {executable}.new
mv -f {executable}.new {executable}
rm -rf "$work"
exec {executable}
"#
    )
}

#[cfg(windows)]
fn windows_update_script(parent_pid: u32, executable: &Path) -> String {
    let executable = powershell_quote(executable);
    format!(
        r#"$ErrorActionPreference = 'Stop'
$work = Join-Path $env:TEMP 'medusa-desktop-main-{parent_pid}'
Remove-Item -LiteralPath $work -Recurse -Force -ErrorAction SilentlyContinue
git clone --depth 1 --branch {BRANCH} '{REPOSITORY_URL}' $work
Set-Location (Join-Path $work 'apps/medusa-desktop')
npm.cmd ci
npm.cmd run build
cargo build --release --manifest-path src-tauri/Cargo.toml
$built = Join-Path $work 'apps/medusa-desktop/src-tauri/target/release/medusa-desktop.exe'
while (Get-Process -Id {parent_pid} -ErrorAction SilentlyContinue) {{ Start-Sleep -Milliseconds 250 }}
Copy-Item -LiteralPath $built -Destination {executable} -Force
Remove-Item -LiteralPath $work -Recurse -Force
Start-Process -FilePath {executable}
"#
    )
}

#[cfg(not(windows))]
fn shell_quote(path: &Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', "'\\''"))
}

#[cfg(windows)]
fn powershell_quote(path: &Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', "''"))
}

fn command_error(error: impl std::fmt::Display) -> MedusaError {
    MedusaError::new(
        ErrorCode::DependencyUnavailable,
        ErrorCategory::Environment,
        format!("could not start the desktop updater: {error}"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(not(windows))]
    #[test]
    fn unix_helper_builds_main_then_replaces_and_restarts() {
        let script = unix_update_script(4242, Path::new("/opt/Medusa/medusa-desktop"));
        assert!(script.contains("git clone --depth 1 --branch main"));
        assert!(script.contains("npm ci"));
        assert!(script.contains("cargo build --release"));
        assert!(script.contains("while kill -0 4242"));
        assert!(script.contains("mv -f"));
        assert!(script.contains("exec '/opt/Medusa/medusa-desktop'"));
    }

    #[cfg(windows)]
    #[test]
    fn windows_helper_builds_main_then_replaces_and_restarts() {
        let script = windows_update_script(
            4242,
            Path::new(r"C:\Program Files\Medusa\medusa-desktop.exe"),
        );
        assert!(script.contains("git clone --depth 1 --branch main"));
        assert!(script.contains("npm.cmd ci"));
        assert!(script.contains("cargo build --release"));
        assert!(script.contains("Get-Process -Id 4242"));
        assert!(script.contains("Copy-Item"));
        assert!(script.contains("Start-Process"));
    }
}
