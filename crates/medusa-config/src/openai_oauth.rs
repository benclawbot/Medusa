//! Shared launch contract for the local ChatGPT/Codex OAuth gateway.
//!
//! The gateway owns OAuth credentials, refresh-token rotation, and the Codex
//! transport. Medusa only needs one stable command contract to start it and
//! one loopback endpoint to probe.

pub const PROVIDER: &str = "openai-oauth";
pub const GATEWAY_BASE_URL: &str = "http://127.0.0.1:10531/v1";
pub const GATEWAY_ADDR: &str = "127.0.0.1:10531";
pub const PACKAGE: &str = "openai-oauth@2.0.0";
pub const LOGIN_TIMEOUT_MS: &str = "300000";

pub const GATEWAY_ARGS: [&str; 4] = ["--yes", PACKAGE, "--no-open", "--detach"];
pub const LOGIN_ARGS: [&str; 6] = [
    "--yes",
    PACKAGE,
    "login",
    "--open",
    "--login-timeout-ms",
    LOGIN_TIMEOUT_MS,
];

pub fn npx_program() -> &'static str {
    if cfg!(windows) { "npx.cmd" } else { "npx" }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gateway_contract_is_pinned_and_headless() {
        assert_eq!(PACKAGE, "openai-oauth@2.0.0");
        assert_eq!(GATEWAY_ARGS, ["--yes", PACKAGE, "--no-open", "--detach"]);
        assert_eq!(LOGIN_ARGS[2], "login");
    }

    #[test]
    fn npx_uses_the_windows_command_wrapper() {
        assert_eq!(npx_program(), if cfg!(windows) { "npx.cmd" } else { "npx" });
    }
}
