//! Shared launch contract for the local ChatGPT/Codex OAuth gateway.
//!
//! The gateway owns OAuth credentials, refresh-token rotation, and the Codex
//! transport. Medusa only needs one stable command contract to start it and
//! one loopback endpoint to probe.

use std::{env, path::PathBuf};

pub const PROVIDER: &str = "openai-oauth";
pub const GATEWAY_BASE_URL: &str = "http://127.0.0.1:10531/v1";
pub const GATEWAY_ADDR: &str = "127.0.0.1:10531";
pub const PACKAGE: &str = "openai-oauth@2.0.0";
pub const LOGIN_TIMEOUT_MS: &str = "300000";

pub const GATEWAY_ARGS: [&str; 4] = ["--yes", PACKAGE, "--no-open", "--detach"];
/// The login helper prints the URL; Medusa opens it itself to avoid the helper's
/// Windows `cmd /c start` argument-splitting bug.
pub const LOGIN_ARGS: [&str; 6] = [
    "--yes",
    PACKAGE,
    "login",
    "--no-open",
    "--login-timeout-ms",
    LOGIN_TIMEOUT_MS,
];

pub fn npx_program() -> &'static str {
    if cfg!(windows) { "npx.cmd" } else { "npx" }
}

/// Returns whether the shared Codex OAuth credential store exists.
///
/// `openai-oauth` owns this file. Medusa only uses its presence to avoid
/// launching a non-interactive overwrite prompt from a hidden child process.
pub fn auth_file_present() -> bool {
    let mut candidates = Vec::new();
    if let Some(home) = env::var_os("CODEX_HOME") {
        candidates.push(PathBuf::from(home).join("auth.json"));
    }
    if let Some(home) = env::var_os("HOME") {
        candidates.push(PathBuf::from(home).join(".codex").join("auth.json"));
    }
    if let Some(home) = env::var_os("USERPROFILE") {
        candidates.push(PathBuf::from(home).join(".codex").join("auth.json"));
    }
    candidates.iter().any(|path| path.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gateway_contract_is_pinned_and_headless() {
        assert_eq!(PACKAGE, "openai-oauth@2.0.0");
        assert_eq!(GATEWAY_ARGS, ["--yes", PACKAGE, "--no-open", "--detach"]);
        assert_eq!(LOGIN_ARGS[2], "login");
        assert_eq!(LOGIN_ARGS[3], "--no-open");
    }

    #[test]
    fn npx_uses_the_windows_command_wrapper() {
        assert_eq!(npx_program(), if cfg!(windows) { "npx.cmd" } else { "npx" });
    }
}
