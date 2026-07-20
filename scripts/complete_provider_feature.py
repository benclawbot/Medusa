from pathlib import Path


def replace(path: str, old: str, new: str) -> None:
    p = Path(path)
    text = p.read_text()
    if new in text:
        return
    if old not in text:
        raise SystemExit(f"expected marker missing in {path}: {old[:80]!r}")
    p.write_text(text.replace(old, new))


config = Path("crates/medusa-config/src/lib.rs")
text = config.read_text()
if "pub base_url: Option<String>" not in text:
    text = text.replace(
        "    pub auto_compact_percent: u8,\n}",
        "    pub auto_compact_percent: u8,\n    pub base_url: Option<String>,\n    pub auth: String,\n    pub speed: String,\n    pub reasoning: String,\n}",
    )
if "base_url: None" not in text:
    text = text.replace(
        "            auto_compact_percent: 40,\n        }",
        "            auto_compact_percent: 40,\n            base_url: None,\n            auth: \"api-key\".into(),\n            speed: \"balanced\".into(),\n            reasoning: \"medium\".into(),\n        }",
    )
# Normalize accidental duplicate insertions from earlier generator runs.
while "        merge_provider_profile(&mut value)?;\n        merge_provider_profile(&mut value)?;" in text:
    text = text.replace(
        "        merge_provider_profile(&mut value)?;\n        merge_provider_profile(&mut value)?;",
        "        merge_provider_profile(&mut value)?;",
    )
if "merge_provider_profile(&mut value)?;" not in text:
    text = text.replace(
        "        let mut value =\n            toml::Value::try_from(Self::default()).map_err(|error| invalid(error.to_string()))?;",
        "        let mut value =\n            toml::Value::try_from(Self::default()).map_err(|error| invalid(error.to_string()))?;\n        merge_provider_profile(&mut value)?;",
    )
if "fn merge_provider_profile" not in text:
    marker = "fn merge_file(base: &mut toml::Value, path: &Path) -> MedusaResult<()> {"
    helper = r'''
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct ProviderProfile {
    connection: String,
    provider: String,
    model: String,
    speed: String,
    reasoning: String,
    auth: String,
    base_url: Option<String>,
    configured: bool,
}

fn provider_profile_path() -> Option<std::path::PathBuf> {
    let base = if cfg!(windows) {
        std::env::var_os("APPDATA").map(std::path::PathBuf::from)
    } else if let Some(path) = std::env::var_os("XDG_CONFIG_HOME") {
        Some(std::path::PathBuf::from(path))
    } else {
        std::env::var_os("HOME").map(|home| std::path::PathBuf::from(home).join(".config"))
    }?;
    Some(base.join("medusa").join("provider.toml"))
}

fn merge_provider_profile(base: &mut toml::Value) -> MedusaResult<()> {
    let Some(path) = provider_profile_path() else { return Ok(()); };
    if !path.exists() { return Ok(()); }
    let text = fs::read_to_string(&path)
        .map_err(|error| invalid(format!("read {}: {error}", path.display())))?;
    let profile: ProviderProfile = toml::from_str(&text)
        .map_err(|error| invalid(format!("parse {}: {error}", path.display())))?;
    if !profile.configured { return Ok(()); }
    let protocol = match profile.connection.as_str() {
        "direct" if matches!(profile.provider.as_str(), "minimax" | "anthropic" | "anthropic-compatible") => "anthropic",
        _ => "openai",
    };
    let overlay = toml::Value::try_from(toml::toml! {
        [model]
        provider = profile.provider
        name = profile.model
        protocol = protocol
        auth = profile.auth
        speed = profile.speed
        reasoning = profile.reasoning
    }).map_err(|error| invalid(error.to_string()))?;
    merge(base, overlay);
    if let Some(url) = profile.base_url {
        set_path(base, "model.base_url", toml::Value::String(url))?;
    }
    Ok(())
}

'''
    text = text.replace(marker, helper + marker)
config.write_text(text)

provider = Path("crates/medusa-provider/src/lib.rs")
ptext = provider.read_text()
if "pub enum ConfiguredProvider" not in ptext:
    insert = r'''

/// Runtime-selected provider supporting Anthropic and OpenAI-compatible APIs.
pub enum ConfiguredProvider {
    Anthropic(MiniMaxProvider),
    OpenAi(OpenAiProvider),
}

impl ConfiguredProvider {
    pub fn from_config(config: &Config) -> MedusaResult<Self> {
        Self::from_config_with_api_key(config, None)
    }

    pub fn from_config_with_api_key(
        config: &Config,
        session_api_key: Option<String>,
    ) -> MedusaResult<Self> {
        if config.model.protocol.eq_ignore_ascii_case("openai") {
            Ok(Self::OpenAi(OpenAiProvider::from_config_with_api_key(config, session_api_key)?))
        } else {
            Ok(Self::Anthropic(MiniMaxProvider::from_config_with_api_key(config, session_api_key)?))
        }
    }
}

impl ModelProvider for ConfiguredProvider {
    fn complete(&self, request: &ModelRequest) -> MedusaResult<ModelResponse> {
        match self {
            Self::Anthropic(provider) => provider.complete(request),
            Self::OpenAi(provider) => provider.complete(request),
        }
    }

    fn capabilities(&self) -> ProviderCapabilities {
        match self {
            Self::Anthropic(provider) => provider.capabilities(),
            Self::OpenAi(provider) => provider.capabilities(),
        }
    }
}

pub struct OpenAiProvider {
    client: Client,
    base_url: String,
    api_key: Option<String>,
    model: String,
    max_retries: u8,
}

impl OpenAiProvider {
    pub fn from_config_with_api_key(config: &Config, session_api_key: Option<String>) -> MedusaResult<Self> {
        let provider = config.model.provider.trim().to_ascii_uppercase().replace('-', "_");
        let api_key = session_api_key
            .or_else(|| env::var(format!("{provider}_API_KEY")).ok())
            .or_else(|| env::var("OPENAI_API_KEY").ok())
            .or_else(|| env::var("MEDUSA_API_KEY").ok());
        if config.model.auth == "api-key" && api_key.is_none() {
            return Err(MedusaError::new(
                ErrorCode::DependencyUnavailable,
                ErrorCategory::Environment,
                format!("missing provider credential; set {provider}_API_KEY, OPENAI_API_KEY, or MEDUSA_API_KEY"),
            ));
        }
        let base_url = config.model.base_url.clone()
            .or_else(|| env::var(format!("{provider}_BASE_URL")).ok())
            .or_else(|| env::var("OPENAI_BASE_URL").ok())
            .or_else(|| env::var("MEDUSA_BASE_URL").ok())
            .unwrap_or_else(|| "https://api.openai.com/v1".to_owned());
        Ok(Self {
            client: shared_http_client()?,
            base_url: base_url.trim_end_matches('/').to_owned(),
            api_key,
            model: config.model.name.clone(),
            max_retries: MAX_PROVIDER_RETRIES,
        })
    }

    fn request_body(&self, request: &ModelRequest) -> Value {
        let mut messages = vec![json!({"role": "system", "content": request.system})];
        for message in &request.messages {
            let role = match message.role { Role::User => "user", Role::Assistant => "assistant" };
            let mut text = String::new();
            let mut tool_calls = Vec::new();
            for block in &message.content {
                match block {
                    MessageBlock::Text { text: value } => text.push_str(value),
                    MessageBlock::ToolUse { id, name, input } => tool_calls.push(json!({
                        "id": id,
                        "type": "function",
                        "function": {"name": name, "arguments": input.to_string()}
                    })),
                    MessageBlock::ToolResult { tool_use_id, content, .. } => messages.push(json!({
                        "role": "tool", "tool_call_id": tool_use_id, "content": content
                    })),
                    MessageBlock::Image { .. } => {}
                }
            }
            let mut wire = json!({"role": role, "content": text});
            if !tool_calls.is_empty() { wire["tool_calls"] = Value::Array(tool_calls); }
            messages.push(wire);
        }
        let tools: Vec<Value> = request.tools.iter().map(|tool| json!({
            "type": "function",
            "function": {"name": tool.name, "description": tool.description, "parameters": tool.input_schema}
        })).collect();
        json!({
            "model": self.model,
            "messages": messages,
            "tools": tools,
            "max_tokens": request.max_tokens,
            "temperature": f64::from(request.temperature_milli) / 1000.0,
            "stream": false
        })
    }
}

impl ModelProvider for OpenAiProvider {
    fn complete(&self, request: &ModelRequest) -> MedusaResult<ModelResponse> {
        let endpoint = format!("{}/chat/completions", self.base_url);
        let body = self.request_body(request);
        let mut attempt = 0_u8;
        loop {
            let mut builder = self.client.post(&endpoint).json(&body);
            if let Some(key) = &self.api_key { builder = builder.bearer_auth(key); }
            match builder.send() {
                Ok(response) if response.status().is_success() => {
                    let wire: OpenAiWireResponse = response.json().map_err(provider_error)?;
                    return wire.into_model_response();
                }
                Ok(response) => {
                    let status = response.status();
                    let text = response.text().unwrap_or_default();
                    let error = classify_status(status, text);
                    if !error.retryable || attempt >= self.max_retries { return Err(error); }
                }
                Err(error) if attempt >= self.max_retries => return Err(provider_error(error)),
                Err(_) => {}
            }
            attempt = attempt.saturating_add(1);
            thread::sleep(Duration::from_millis(250 * u64::from(attempt)));
        }
    }
}

#[derive(Debug, Deserialize)]
struct OpenAiWireResponse {
    id: Option<String>,
    #[serde(default)]
    choices: Vec<OpenAiChoice>,
    #[serde(default)]
    usage: OpenAiUsage,
}
#[derive(Debug, Deserialize)]
struct OpenAiChoice { message: OpenAiMessage, finish_reason: Option<String> }
#[derive(Debug, Deserialize)]
struct OpenAiMessage {
    content: Option<String>,
    #[serde(default)]
    tool_calls: Vec<OpenAiToolCall>,
}
#[derive(Debug, Deserialize)]
struct OpenAiToolCall { id: String, function: OpenAiFunction }
#[derive(Debug, Deserialize)]
struct OpenAiFunction { name: String, arguments: String }
#[derive(Debug, Default, Deserialize)]
struct OpenAiUsage {
    #[serde(default)] prompt_tokens: u64,
    #[serde(default)] completion_tokens: u64,
}
impl OpenAiWireResponse {
    fn into_model_response(self) -> MedusaResult<ModelResponse> {
        let choice = self.choices.into_iter().next().ok_or_else(|| MedusaError::new(
            ErrorCode::DependencyUnavailable, ErrorCategory::Execution, "provider returned no choices"
        ))?;
        let mut blocks = Vec::new();
        if let Some(text) = choice.message.content.filter(|value| !value.is_empty()) {
            blocks.push(ResponseBlock::Text { text });
        }
        for call in choice.message.tool_calls {
            let input = serde_json::from_str(&call.function.arguments).map_err(provider_error)?;
            blocks.push(ResponseBlock::ToolUse { id: call.id, name: call.function.name, input });
        }
        Ok(ModelResponse {
            response_id: self.id,
            stop_reason: choice.finish_reason,
            blocks,
            usage: Usage { input_tokens: self.usage.prompt_tokens, output_tokens: self.usage.completion_tokens, ..Usage::default() },
        })
    }
}
'''
    marker = "#[cfg(test)]\nmod tests"
    ptext = ptext.replace(marker, insert + "\n" + marker, 1) if marker in ptext else ptext + insert
provider.write_text(ptext)

for file in [Path("crates/medusa-runtime/src/lib.rs"), Path("crates/medusa-cli/src/main.rs")]:
    text = file.read_text().replace("MiniMaxProvider", "ConfiguredProvider")
    file.write_text(text)

main = Path("crates/medusa-cli/src/main.rs")
text = main.read_text()
text = text.replace('ok: std::env::var("MINIMAX_API_KEY").is_ok(),', 'ok: config.model.auth != "api-key" || provider_credential_present(config),')
text = text.replace(
    'detail: if std::env::var("MINIMAX_API_KEY").is_ok() {\n                "MINIMAX_API_KEY is present".into()\n            } else {\n                "MINIMAX_API_KEY is absent; direct MiniMax live runs are unavailable".into()\n            },',
    'detail: provider_credential_detail(config),',
)
if "fn provider_credential_present" not in text:
    marker = "fn command_check(name: &'static str, program: &str, args: &[&str]) -> DoctorCheck {"
    helper = r'''
fn provider_credential_present(config: &Config) -> bool {
    if config.model.auth != "api-key" { return true; }
    let prefix = config.model.provider.trim().to_ascii_uppercase().replace('-', "_");
    std::env::var(format!("{prefix}_API_KEY")).is_ok()
        || std::env::var("OPENAI_API_KEY").is_ok()
        || std::env::var("MEDUSA_API_KEY").is_ok()
        || std::env::var("MINIMAX_API_KEY").is_ok()
        || std::env::var("ANTHROPIC_API_KEY").is_ok()
}

fn provider_credential_detail(config: &Config) -> String {
    if config.model.auth != "api-key" { return format!("authentication mode: {}", config.model.auth); }
    if provider_credential_present(config) {
        "provider credential is present".to_owned()
    } else {
        "provider credential is absent; configure the provider-specific API key environment variable".to_owned()
    }
}

'''
    text = text.replace(marker, helper + marker)
main.write_text(text)

readme = Path("README.md")
rtext = readme.read_text()
section = r'''

## Provider configuration

On first interactive launch, Medusa creates a non-secret provider profile in the platform user configuration directory. The profile controls the real runtime used by the TUI and headless commands.

Supported protocols:

- Anthropic Messages API: MiniMax, Anthropic, and compatible endpoints
- OpenAI Chat Completions API: OpenAI-compatible gateways, OmniRoute, Ollama-compatible servers, and local endpoints

Credentials are never written to `provider.toml`. Use a provider-specific `<PROVIDER>_API_KEY`, `OPENAI_API_KEY`, `MEDUSA_API_KEY`, or the selected gateway's existing authentication. `medusa config show` displays only non-secret settings and `medusa config reset` removes the profile.
'''
if "## Provider configuration" not in rtext:
    readme.write_text(rtext.rstrip() + section + "\n")

print("provider feature completion applied")
