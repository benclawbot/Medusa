use std::{
    env, fs,
    path::{Path, PathBuf},
};

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use medusa_update::MainBranchUpdater;
use serde::Serialize;

use crate::desktop_command::hidden_command;

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

#[tauri::command(rename_all = "camelCase")]
pub fn desktop_update_from_main(app: tauri::AppHandle, target_sha: String) -> Result<(), String> {
    schedule_update(&app, &target_sha).map_err(|error| error.to_string())?;
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

fn schedule_update(app: &tauri::AppHandle, target_sha: &str) -> MedusaResult<()> {
    let target_sha = validate_target_sha(target_sha)?;
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
        fs::write(&helper, windows_update_script(parent_pid, &executable, target_sha))?;
        hidden_command("powershell")
            .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
            .arg(&helper)
            .spawn()
            .map_err(command_error)?;
    }

    #[cfg(not(windows))]
    {
        fs::write(&helper, unix_update_script(parent_pid, &executable, target_sha))?;
        hidden_command("sh")
            .arg(&helper)
            .spawn()
            .map_err(command_error)?;
    }

    let _ = app;
    Ok(())
}

fn validate_target_sha(target_sha: &str) -> MedusaResult<&str> {
    if target_sha.len() == 40 && target_sha.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Ok(target_sha);
    }
    Err(MedusaError::new(
        ErrorCode::InvalidConfiguration,
        ErrorCategory::Validation,
        "desktop update target must be a full 40-character Git commit SHA",
    ))
}

fn missing_dependencies() -> Vec<String> {
    ["git", "npm", "cargo"]
        .into_iter()
        .filter(|program| !command_available(program))
        .map(str::to_owned)
        .collect()
}

fn command_available(program: &str) -> bool {
    command_candidates(program)
        .into_iter()
        .any(|candidate| command_succeeds(&candidate))
}

fn command_succeeds(program: &Path) -> bool {
    hidden_command(program)
        .arg("--version")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

#[cfg(not(windows))]
fn command_candidates(program: &str) -> Vec<PathBuf> {
    vec![PathBuf::from(program)]
}

#[cfg(windows)]
fn command_candidates(program: &str) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if program.eq_ignore_ascii_case("npm") {
        candidates.push(PathBuf::from("npm.cmd"));
        for root in [env::var_os("ProgramFiles"), env::var_os("ProgramW6432")]
            .into_iter()
            .flatten()
        {
            candidates.push(PathBuf::from(root).join("nodejs").join("npm.cmd"));
        }
        if let Some(local_app_data) = env::var_os("LOCALAPPDATA") {
            candidates.push(
                PathBuf::from(local_app_data)
                    .join("Programs")
                    .join("nodejs")
                    .join("npm.cmd"),
            );
        }
    } else {
        candidates.push(PathBuf::from(program));
        candidates.push(PathBuf::from(format!("{program}.exe")));
    }
    candidates
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
fn unix_update_script(parent_pid: u32, executable: &Path, target_sha: &str) -> String {
    let executable = shell_quote(executable);
    format!(
        r#"#!/bin/sh
set -eu
work="${{TMPDIR:-/tmp}}/medusa-desktop-main-{parent_pid}"
rm -rf "$work"
git init -q "$work"
git -C "$work" remote add origin '{REPOSITORY_URL}'
git -C "$work" fetch --depth 1 origin '{target_sha}'
git -C "$work" checkout --detach -q '{target_sha}'
actual_sha="$(git -C "$work" rev-parse HEAD)"
if [ "$actual_sha" != '{target_sha}' ]; then
  echo "desktop update revision mismatch: expected {target_sha}, got $actual_sha" >&2
  exit 1
fi
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

#[cfg(any(windows, test))]
fn windows_update_script(parent_pid: u32, executable: &Path, target_sha: &str) -> String {
    let executable = powershell_quote(executable);
    format!(
        r#"$ErrorActionPreference = 'Stop'
$machinePath = [Environment]::GetEnvironmentVariable('Path', 'Machine')
$userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
$env:Path = @($machinePath, $userPath, $env:Path) -join ';'
$npm = Get-Command npm.cmd -ErrorAction SilentlyContinue
if (-not $npm) {{
    $npmCandidates = @(
        (Join-Path $env:ProgramFiles 'nodejs\npm.cmd'),
        (Join-Path $env:LOCALAPPDATA 'Programs\nodejs\npm.cmd')
    )
    $npmPath = $npmCandidates | Where-Object {{ Test-Path -LiteralPath $_ }} | Select-Object -First 1
    if (-not $npmPath) {{ throw 'npm.cmd was not found after refreshing PATH' }}
}} else {{
    $npmPath = $npm.Source
}}
$work = Join-Path $env:TEMP 'medusa-desktop-main-{parent_pid}'
Remove-Item -LiteralPath $work -Recurse -Force -ErrorAction SilentlyContinue
git init -q $work
git -C $work remote add origin '{REPOSITORY_URL}'
git -C $work fetch --depth 1 origin '{target_sha}'
git -C $work checkout --detach -q '{target_sha}'
$actualSha = (git -C $work rev-parse HEAD).Trim()
if ($actualSha -ne '{target_sha}') {{
    throw "desktop update revision mismatch: expected {target_sha}, got $actualSha"
}}
Set-Location (Join-Path $work 'apps/medusa-desktop')
& $npmPath ci
& $npmPath run build
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

#[cfg(any(windows, test))]
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

    const TARGET_SHA: &str = "0123456789abcdef0123456789abcdef01234567";

    #[test]
    fn update_target_requires_full_commit_sha() {
        assert_eq!(validate_target_sha(TARGET_SHA).unwrap(), TARGET_SHA);
        assert!(validate_target_sha("main").is_err());
        assert!(validate_target_sha("0123456789abcdef0123456789abcdef0123456z").is_err());
        assert!(validate_target_sha("01234567;rm -rf /").is_err());
    }

    #[cfg(not(windows))]
    #[test]
    fn unix_helper_builds_preflight_revision_then_replaces_and_restarts() {
        let script = unix_update_script(
            4242,
            Path::new("/opt/Medusa/medusa-desktop"),
            TARGET_SHA,
        );
        assert!(script.contains(&format!("fetch --depth 1 origin '{TARGET_SHA}'")));
        assert!(script.contains(&format!("checkout --detach -q '{TARGET_SHA}'")));
        assert!(script.contains("rev-parse HEAD"));
        assert!(script.contains(&format!("!= '{TARGET_SHA}'")));
        assert!(!script.contains("--branch main"));
        assert!(script.contains("npm ci"));
        assert!(script.contains("cargo build --release"));
        assert!(script.contains("while kill -0 4242"));
        assert!(script.contains("mv -f"));
        assert!(script.contains("exec '/opt/Medusa/medusa-desktop'"));
    }

    #[test]
    fn windows_helper_builds_preflight_revision_even_if_main_moves() {
        let script = windows_update_script(
            4242,
            Path::new(r"C:\Program Files\Medusa\medusa-desktop.exe"),
            TARGET_SHA,
        );
        assert!(script.contains("GetEnvironmentVariable('Path', 'Machine')"));
        assert!(script.contains("Get-Command npm.cmd"));
        assert!(script.contains(r"nodejs\npm.cmd"));
        assert!(script.contains("& $npmPath ci"));
        assert!(script.contains("& $npmPath run build"));
        assert!(script.contains(&format!("fetch --depth 1 origin '{TARGET_SHA}'")));
        assert!(script.contains(&format!("checkout --detach -q '{TARGET_SHA}'")));
        assert!(script.contains("rev-parse HEAD"));
        assert!(script.contains(&format!("-ne '{TARGET_SHA}'")));
        assert!(!script.contains("--branch main"));
        assert!(script.contains("cargo build --release"));
        assert!(script.contains("Get-Process -Id 4242"));
        assert!(script.contains("Copy-Item"));
        assert!(script.contains("Start-Process"));
    }

    #[cfg(windows)]
    #[test]
    fn npm_detection_includes_cmd_and_standard_node_locations() {
        let candidates = command_candidates("npm");
        assert!(candidates.iter().any(|path| path == Path::new("npm.cmd")));
        assert!(
            candidates
                .iter()
                .any(|path| path.ends_with(r"nodejs\npm.cmd"))
        );
    }
}
