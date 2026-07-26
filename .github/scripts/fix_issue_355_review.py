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


# Wire the production config setting into the verification decision.
replace_once(
    "crates/medusa-agent/src/verification.rs",
    "pub fn targeted_verification(repo: &Path) -> MedusaResult<VerificationResult> {\n    targeted_verification_for_paths(repo, &[])\n}",
    "pub fn targeted_verification(repo: &Path) -> MedusaResult<VerificationResult> {\n    targeted_verification_for_paths(repo, &[], true)\n}",
    "public verification entrypoint",
)
replace_once(
    "crates/medusa-agent/src/verification.rs",
    "pub(crate) fn targeted_verification_for_paths(\n    repo: &Path,\n    artifact_paths: &[String],\n) -> MedusaResult<VerificationResult> {\n    let changed_paths = artifact_paths.iter().map(PathBuf::from).collect::<Vec<_>>();\n    let browser_decision = browser_verification_decision(&changed_paths);",
    "pub(crate) fn targeted_verification_for_paths(\n    repo: &Path,\n    artifact_paths: &[String],\n    browser_on_ui_change: bool,\n) -> MedusaResult<VerificationResult> {\n    let changed_paths = artifact_paths.iter().map(PathBuf::from).collect::<Vec<_>>();\n    let browser_decision = browser_verification_decision(&changed_paths, browser_on_ui_change);",
    "verification config plumbing",
)
replace_once(
    "crates/medusa-agent/src/verification.rs",
    "fn browser_verification_decision(changed_paths: &[PathBuf]) -> BrowserVerificationDecision {",
    "fn browser_verification_decision(\n    changed_paths: &[PathBuf],\n    browser_on_ui_change: bool,\n) -> BrowserVerificationDecision {",
    "browser decision signature",
)
replace_once(
    "crates/medusa-agent/src/verification.rs",
    "        _ if changed_paths\n            .iter()\n            .any(|path| is_effective_ui_change(path)) =>",
    "        _ if browser_on_ui_change\n            && changed_paths\n                .iter()\n                .any(|path| is_effective_ui_change(path)) =>",
    "browser config guard",
)
# Update focused tests.
p = Path("crates/medusa-agent/src/verification.rs")
t = p.read_text()
t = t.replace(
    'browser_verification_decision(&[PathBuf::from("README.md")])',
    'browser_verification_decision(&[PathBuf::from("README.md")], true)',
).replace(
    'browser_verification_decision(&[PathBuf::from("apps/web/App.tsx")])',
    'browser_verification_decision(&[PathBuf::from("apps/web/App.tsx")], true)',
)
if "configuration_disables_automatic_browser_verification" not in t:
    marker = "    #[test]\n    fn manual_override_is_auditable() {"
    test = '''    #[test]\n    fn configuration_disables_automatic_browser_verification() {\n        assert_eq!(\n            browser_verification_decision(&[PathBuf::from("apps/web/App.tsx")], false),\n            BrowserVerificationDecision::Skip\n        );\n    }\n\n'''
    t = t.replace(marker, test + marker, 1)
p.write_text(t)

replace_once(
    "crates/medusa-agent/src/engine.rs",
    "            let mut verification = targeted_verification_for_paths(\n                &session.repo,\n                &successful_mutation_paths(session),\n            )?;",
    "            let mut verification = targeted_verification_for_paths(\n                &session.repo,\n                &successful_mutation_paths(session),\n                self.config.verification.browser_on_ui_change,\n            )?;",
    "agent config callsite",
)

# Extract paths from all recognized mutating tools, including symbol rename and Desktop Commander.
replace_once(
    "crates/medusa-agent/src/engine_support.rs",
    "                        _ => {}\n                    }",
    '''                        "symbol_rename" => collect_mutation_paths(input, &mut paths),\n                        "desktop_commander" => {\n                            if input\n                                .get("tool")\n                                .and_then(serde_json::Value::as_str)\n                                .is_some_and(desktop_commander_tool_is_mutating)\n                            {\n                                if let Some(arguments) = input.get("arguments") {\n                                    collect_mutation_paths(arguments, &mut paths);\n                                }\n                            }\n                        }\n                        _ => {}\n                    }''',
    "mutation path dispatch",
)
replace_once(
    "crates/medusa-agent/src/engine_support.rs",
    "pub(crate) fn plan_is_complete(session: &AgentSession) -> bool {",
    '''fn collect_mutation_paths(value: &serde_json::Value, paths: &mut Vec<String>) {\n    match value {\n        serde_json::Value::Object(map) => {\n            for (key, value) in map {\n                let path_key = matches!(\n                    key.as_str(),\n                    "path" | "file" | "file_path" | "source" | "destination" | "old_path" | "new_path"\n                );\n                if path_key {\n                    if let Some(path) = value.as_str() {\n                        paths.push(path.to_owned());\n                        continue;\n                    }\n                }\n                collect_mutation_paths(value, paths);\n            }\n        }\n        serde_json::Value::Array(values) => {\n            for value in values {\n                collect_mutation_paths(value, paths);\n            }\n        }\n        _ => {}\n    }\n}\n\npub(crate) fn plan_is_complete(session: &AgentSession) -> bool {''',
    "recursive mutation path collector",
)

# Let verification spawn a sidecar explicitly scoped to loopback development routes.
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
# rustfmt will normalize the map_err indentation.

# Browser daemon allows only explicit loopback targets when verification opts in.
replace_once(
    "crates/medusa-browserd/src/validation.rs",
    "pub fn validate_public_url(url: &url::Url) -> Result<(), String> {\n    let host = url",
    '''pub fn validate_public_url(url: &url::Url) -> Result<(), String> {\n    if std::env::var_os("MEDUSA_BROWSER_ALLOW_LOOPBACK").is_some()\n        && validate_loopback_url(url).is_ok()\n    {\n        return Ok(());\n    }\n    let host = url''',
    "loopback validation gate",
)
replace_once(
    "crates/medusa-browserd/src/validation.rs",
    "#[cfg(test)]\nmod tests {",
    '''pub(crate) fn validate_loopback_url(url: &url::Url) -> Result<(), String> {\n    if !matches!(url.scheme(), "http" | "https") {\n        return Err("local browser URLs must use http or https".to_owned());\n    }\n    if !url.username().is_empty() || url.password().is_some() {\n        return Err("local browser URLs must not include credentials".to_owned());\n    }\n    let host = url\n        .host_str()\n        .ok_or_else(|| "local browser URL must include a host".to_owned())?;\n    let is_loopback = host.eq_ignore_ascii_case("localhost")\n        || host\n            .parse::<std::net::IpAddr>()\n            .is_ok_and(|address| address.is_loopback());\n    if !is_loopback {\n        return Err("local browser URL must target loopback".to_owned());\n    }\n    Ok(())\n}\n\n#[cfg(test)]\nmod tests {''',
    "loopback validator",
)

# Apply the same opt-in to the pinned proxy; private LAN ranges remain denied.
replace_once(
    "crates/medusa-browserd/src/proxy.rs",
    "use medusa_browser_client::network_policy::{ResolvedTarget, resolve_public_target};",
    "use medusa_browser_client::network_policy::{ResolvedTarget, resolve_public_target};\n\nuse crate::validation::validate_loopback_url;",
    "proxy loopback import",
)
replace_once(
    "crates/medusa-browserd/src/proxy.rs",
    "fn resolve_url(url: &url::Url) -> Result<ResolvedTarget, String> {\n    let host = url",
    '''fn resolve_url(url: &url::Url) -> Result<ResolvedTarget, String> {\n    if std::env::var_os("MEDUSA_BROWSER_ALLOW_LOOPBACK").is_some() {\n        if validate_loopback_url(url).is_ok() {\n            let host = url.host_str().ok_or_else(|| "local browser URL must include a host".to_owned())?;\n            let port = url.port_or_known_default().unwrap_or(80);\n            let addresses = (host, port)\n                .to_socket_addrs()\n                .map_err(|error| error.to_string())?\n                .filter(|address| address.ip().is_loopback())\n                .collect::<Vec<_>>();\n            if addresses.is_empty() {\n                return Err("local browser URL did not resolve to loopback".to_owned());\n            }\n            return Ok(ResolvedTarget::new_for_loopback(url.scheme(), host, port, addresses));\n        }\n    }\n    let host = url''',
    "proxy loopback resolution",
)
# Import ToSocketAddrs.
p = Path("crates/medusa-browserd/src/proxy.rs")
t = p.read_text().replace(
    "net::{Shutdown, SocketAddr, TcpListener, TcpStream},",
    "net::{Shutdown, SocketAddr, TcpListener, TcpStream, ToSocketAddrs},",
)
p.write_text(t)

# Add a constrained constructor for already-validated loopback targets.
replace_once(
    "crates/medusa-browser-client/src/network_policy.rs",
    "impl ResolvedTarget {",
    '''impl ResolvedTarget {\n    #[must_use]\n    pub fn new_for_loopback(\n        scheme: &str,\n        host: &str,\n        port: u16,\n        addresses: Vec<std::net::SocketAddr>,\n    ) -> Self {\n        Self {\n            scheme: scheme.to_owned(),\n            host: host.to_owned(),\n            port,\n            addresses,\n        }\n    }''',
    "loopback resolved target constructor",
)

# Capture console.error, page errors, and unhandled rejections before navigation.
replace_once(
    "browser/playwright_bridge.mjs",
    "    context = await browser.newContext({ serviceWorkers: 'block' });\n    page = await context.newPage();",
    '''    context = await browser.newContext({ serviceWorkers: 'block' });\n    await context.addInitScript(() => {\n      globalThis.__MEDUSA_CONSOLE_ERRORS__ = [];\n      const originalError = console.error.bind(console);\n      console.error = (...args) => {\n        globalThis.__MEDUSA_CONSOLE_ERRORS__.push(args.map(String).join(' '));\n        originalError(...args);\n      };\n      addEventListener('error', (event) => {\n        globalThis.__MEDUSA_CONSOLE_ERRORS__.push(String(event.error?.stack ?? event.message));\n      });\n      addEventListener('unhandledrejection', (event) => {\n        globalThis.__MEDUSA_CONSOLE_ERRORS__.push(String(event.reason?.stack ?? event.reason));\n      });\n    });\n    page = await context.newPage();''',
    "console capture init script",
)

print("issue 355 review fixes applied")
