//! Shared provider identity for direct ChatGPT OAuth through Codex app-server.
//!
//! The Codex CLI owns the OAuth credential store, browser callback, refresh lifecycle, and
//! upstream transport. Medusa only passes this provider id to the runtime app-server client.

pub const PROVIDER: &str = "openai-oauth";
pub const CONNECTION: &str = "chatgpt-oauth";
pub const APP_SERVER_ARGS: [&str; 2] = ["app-server", "--stdio"];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_server_contract_is_direct_and_stdio_based() {
        assert_eq!(PROVIDER, "openai-oauth");
        assert_eq!(CONNECTION, "chatgpt-oauth");
        assert_eq!(APP_SERVER_ARGS, ["app-server", "--stdio"]);
    }
}
