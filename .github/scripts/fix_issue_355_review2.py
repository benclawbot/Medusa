from pathlib import Path


def replace_once(path: str, old: str, new: str, label: str) -> None:
    file = Path(path)
    text = file.read_text()
    if new in text:
        return
    count = text.count(old)
    if count != 1:
        raise SystemExit(f"{label}: expected one match, found {count}")
    file.write_text(text.replace(old, new, 1))


replace_once(
    "crates/medusa-agent/src/engine_support.rs",
    "                        _ => {}\n                    }",
    '''                        "symbol_rename" => collect_mutation_paths(input, &mut paths),
                        "desktop_commander" => {
                            if input
                                .get("tool")
                                .and_then(serde_json::Value::as_str)
                                .is_some_and(desktop_commander_tool_is_mutating)
                            {
                                if let Some(arguments) = input.get("arguments") {
                                    collect_mutation_paths(arguments, &mut paths);
                                }
                            }
                        }
                        _ => {}
                    }''',
    "mutation path dispatch",
)
replace_once(
    "crates/medusa-agent/src/engine_support.rs",
    "pub(crate) fn plan_is_complete(session: &AgentSession) -> bool {",
    '''fn collect_mutation_paths(value: &serde_json::Value, paths: &mut Vec<String>) {
    match value {
        serde_json::Value::Object(map) => {
            for (key, value) in map {
                if matches!(
                    key.as_str(),
                    "path" | "file" | "file_path" | "source" | "destination" | "old_path" | "new_path"
                ) {
                    if let Some(path) = value.as_str() {
                        paths.push(path.to_owned());
                        continue;
                    }
                }
                collect_mutation_paths(value, paths);
            }
        }
        serde_json::Value::Array(values) => {
            for value in values {
                collect_mutation_paths(value, paths);
            }
        }
        _ => {}
    }
}

pub(crate) fn plan_is_complete(session: &AgentSession) -> bool {''',
    "recursive mutation path collector",
)

replace_once(
    "crates/medusa-browser-client/src/lib.rs",
    "    pub fn spawn(command: &str) -> MedusaResult<Self> {\n        let mut child = Command::new(command)\n            .arg(\"--stdio\")",
    "    pub fn spawn(command: &str) -> MedusaResult<Self> {\n        Self::spawn_with_env(command, &[])\n    }\n\n    pub fn spawn_with_env(command: &str, environment: &[(&str, &str)]) -> MedusaResult<Self> {\n        let mut command_builder = Command::new(command);\n        command_builder.arg(\"--stdio\");\n        for (key, value) in environment {\n            command_builder.env(key, value);\n        }\n        let mut child = command_builder",
    "browser client scoped environment",
)
replace_once(
    "crates/medusa-agent/src/verification.rs",
    "    let mut client = BrowserClient::spawn(&command).map_err(|error| {",
    "    let mut client = BrowserClient::spawn_with_env(\n        &command,\n        &[(\"MEDUSA_BROWSER_ALLOW_LOOPBACK\", \"1\")],\n    )\n    .map_err(|error| {",
    "verification loopback sidecar",
)

replace_once(
    "crates/medusa-browserd/src/validation.rs",
    "pub fn validate_public_url(url: &url::Url) -> Result<(), String> {\n    let host = url",
    '''pub fn validate_public_url(url: &url::Url) -> Result<(), String> {
    if std::env::var_os("MEDUSA_BROWSER_ALLOW_LOOPBACK").is_some()
        && validate_loopback_url(url).is_ok()
    {
        return Ok(());
    }
    let host = url''',
    "loopback validation gate",
)
replace_once(
    "crates/medusa-browserd/src/validation.rs",
    "#[cfg(test)]\nmod tests {",
    '''pub(crate) fn validate_loopback_url(url: &url::Url) -> Result<(), String> {
    if !matches!(url.scheme(), "http" | "https") {
        return Err("local browser URLs must use http or https".to_owned());
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("local browser URLs must not include credentials".to_owned());
    }
    let host = url
        .host_str()
        .ok_or_else(|| "local browser URL must include a host".to_owned())?;
    let loopback = host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|address| address.is_loopback());
    loopback
        .then_some(())
        .ok_or_else(|| "local browser URL must target loopback".to_owned())
}

#[cfg(test)]
mod tests {''',
    "loopback validator",
)

replace_once(
    "crates/medusa-browserd/src/proxy.rs",
    "use medusa_browser_client::network_policy::{ResolvedTarget, resolve_public_target};",
    "use medusa_browser_client::network_policy::{ResolvedTarget, resolve_public_target};\n\nuse crate::validation::validate_loopback_url;",
    "proxy loopback import",
)
p = Path("crates/medusa-browserd/src/proxy.rs")
t = p.read_text().replace(
    "net::{Shutdown, SocketAddr, TcpListener, TcpStream},",
    "net::{Shutdown, SocketAddr, TcpListener, TcpStream, ToSocketAddrs},",
)
p.write_text(t)
replace_once(
    "crates/medusa-browserd/src/proxy.rs",
    "fn resolve_url(url: &url::Url) -> Result<ResolvedTarget, String> {\n    let host = url",
    '''fn resolve_url(url: &url::Url) -> Result<ResolvedTarget, String> {
    if std::env::var_os("MEDUSA_BROWSER_ALLOW_LOOPBACK").is_some()
        && validate_loopback_url(url).is_ok()
    {
        let host = url
            .host_str()
            .ok_or_else(|| "local browser URL must include a host".to_owned())?;
        let port = url.port_or_known_default().unwrap_or(80);
        let addresses = (host, port)
            .to_socket_addrs()
            .map_err(|error| error.to_string())?
            .filter(|address| address.ip().is_loopback())
            .collect::<Vec<_>>();
        if addresses.is_empty() {
            return Err("local browser URL did not resolve to loopback".to_owned());
        }
        return Ok(ResolvedTarget::new_for_loopback(
            url.scheme(),
            host,
            port,
            addresses,
        ));
    }
    let host = url''',
    "proxy loopback resolution",
)
replace_once(
    "crates/medusa-browser-client/src/network_policy.rs",
    "impl ResolvedTarget {",
    '''impl ResolvedTarget {
    #[must_use]
    pub fn new_for_loopback(
        scheme: &str,
        host: &str,
        port: u16,
        addresses: Vec<std::net::SocketAddr>,
    ) -> Self {
        Self {
            scheme: scheme.to_owned(),
            host: host.to_owned(),
            port,
            addresses,
        }
    }''',
    "loopback resolved target constructor",
)

replace_once(
    "browser/playwright_bridge.mjs",
    "    context = await browser.newContext({ serviceWorkers: 'block' });\n    page = await context.newPage();",
    '''    context = await browser.newContext({ serviceWorkers: 'block' });
    await context.addInitScript(() => {
      globalThis.__MEDUSA_CONSOLE_ERRORS__ = [];
      const originalError = console.error.bind(console);
      console.error = (...args) => {
        globalThis.__MEDUSA_CONSOLE_ERRORS__.push(args.map(String).join(' '));
        originalError(...args);
      };
      addEventListener('error', (event) => {
        globalThis.__MEDUSA_CONSOLE_ERRORS__.push(String(event.error?.stack ?? event.message));
      });
      addEventListener('unhandledrejection', (event) => {
        globalThis.__MEDUSA_CONSOLE_ERRORS__.push(String(event.reason?.stack ?? event.reason));
      });
    });
    page = await context.newPage();''',
    "console capture init script",
)

print("issue 355 remaining review fixes applied")
