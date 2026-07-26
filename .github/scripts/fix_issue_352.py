from pathlib import Path


def replace_once(text: str, old: str, new: str, label: str) -> str:
    count = text.count(old)
    if count != 1:
        raise SystemExit(f"{label}: expected exactly one match, found {count}")
    return text.replace(old, new, 1)


config_path = Path("crates/medusa-config/src/lib.rs")
text = config_path.read_text()
text = replace_once(
    text,
    "    pub fallback_providers: Vec<String>,\n    pub name: String,\n    pub protocol: String,\n",
    "    pub fallback_providers: Vec<FallbackProviderConfig>,\n    pub name: String,\n    pub protocol: String,\n",
    "fallback provider type",
)
text = replace_once(
    text,
    "    pub base_url: Option<String>,\n    pub auth: String,\n}\n\n/// Memory settings",
    "    pub base_url: Option<String>,\n    pub auth: String,\n    pub tool_calling: bool,\n    pub streaming: bool,\n    pub max_retries: u8,\n    pub retry_base_delay_ms: u64,\n    pub retry_max_delay_ms: u64,\n    pub retry_jitter_ms: u64,\n}\n\n/// A complete, independently resolved fallback route.\n#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]\n#[serde(deny_unknown_fields)]\npub struct FallbackProviderConfig {\n    pub provider: String,\n    pub name: String,\n    pub protocol: String,\n    pub base_url: Option<String>,\n    pub auth: String,\n    #[serde(default = \"default_true\")]\n    pub tool_calling: bool,\n    #[serde(default)]\n    pub streaming: bool,\n    #[serde(default = \"default_max_retries\")]\n    pub max_retries: u8,\n    #[serde(default = \"default_retry_base_delay_ms\")]\n    pub retry_base_delay_ms: u64,\n    #[serde(default = \"default_retry_max_delay_ms\")]\n    pub retry_max_delay_ms: u64,\n    #[serde(default = \"default_retry_jitter_ms\")]\n    pub retry_jitter_ms: u64,\n}\n\nfn default_true() -> bool {\n    true\n}\n\nfn default_max_retries() -> u8 {\n    1\n}\n\nfn default_retry_base_delay_ms() -> u64 {\n    250\n}\n\nfn default_retry_max_delay_ms() -> u64 {\n    8_000\n}\n\nfn default_retry_jitter_ms() -> u64 {\n    100\n}\n\n/// Memory settings",
    "fallback profile definition",
)
text = replace_once(
    text,
    "            base_url: None,\n            auth: \"api-key\".into(),\n        }",
    "            base_url: None,\n            auth: \"api-key\".into(),\n            tool_calling: true,\n            streaming: false,\n            max_retries: default_max_retries(),\n            retry_base_delay_ms: default_retry_base_delay_ms(),\n            retry_max_delay_ms: default_retry_max_delay_ms(),\n            retry_jitter_ms: default_retry_jitter_ms(),\n        }",
    "model defaults",
)
text = replace_once(
    text,
    "        if self.model.context_window_tokens == 0 {\n            return Err(invalid(\"context_window_tokens must be greater than zero\"));\n        }",
    "        if self.model.context_window_tokens == 0 {\n            return Err(invalid(\"context_window_tokens must be greater than zero\"));\n        }\n        validate_route(\n            \"primary\",\n            &self.model.provider,\n            &self.model.name,\n            &self.model.protocol,\n            &self.model.auth,\n            self.model.max_retries,\n            self.model.retry_base_delay_ms,\n            self.model.retry_max_delay_ms,\n            self.model.retry_jitter_ms,\n        )?;\n        for (index, fallback) in self.model.fallback_providers.iter().enumerate() {\n            validate_route(\n                &format!(\"fallback[{index}]\"),\n                &fallback.provider,\n                &fallback.name,\n                &fallback.protocol,\n                &fallback.auth,\n                fallback.max_retries,\n                fallback.retry_base_delay_ms,\n                fallback.retry_max_delay_ms,\n                fallback.retry_jitter_ms,\n            )?;\n        }",
    "route validation call",
)
text = replace_once(
    text,
    "fn invalid(message: impl Into<String>) -> MedusaError {",
    "fn validate_route(\n    label: &str,\n    provider: &str,\n    model: &str,\n    protocol: &str,\n    auth: &str,\n    max_retries: u8,\n    base_delay_ms: u64,\n    max_delay_ms: u64,\n    jitter_ms: u64,\n) -> MedusaResult<()> {\n    if provider.trim().is_empty() || model.trim().is_empty() {\n        return Err(invalid(format!(\"{label} provider and model must be explicit\")));\n    }\n    if !matches!(protocol.trim().to_ascii_lowercase().as_str(), \"anthropic\" | \"openai\") {\n        return Err(invalid(format!(\"{label} protocol must be anthropic or openai\")));\n    }\n    if !matches!(auth.trim().to_ascii_lowercase().as_str(), \"api-key\" | \"none\") {\n        return Err(invalid(format!(\"{label} auth must be api-key or none\")));\n    }\n    if max_retries > 8 {\n        return Err(invalid(format!(\"{label} max_retries must be at most 8\")));\n    }\n    if base_delay_ms == 0 || max_delay_ms < base_delay_ms || jitter_ms > max_delay_ms {\n        return Err(invalid(format!(\"{label} retry policy is invalid or unbounded\")));\n    }\n    Ok(())\n}\n\nfn invalid(message: impl Into<String>) -> MedusaError {",
    "route validation function",
)
config_path.write_text(text)

provider_path = Path("crates/medusa-provider/src/lib.rs")
text = provider_path.read_text()
text = replace_once(text, "use medusa_config::Config;", "use medusa_config::{Config, FallbackProviderConfig};", "config import")
text = replace_once(
    text,
    "pub use manager::{ProviderHealth, ProviderManager};",
    "pub use manager::{ProviderHealth, ProviderManager, ProviderRouteProfile, RouteRetryPolicy};",
    "manager exports",
)
text = replace_once(
    text,
    "    pub max_images_per_request: Option<u32>,\n}",
    "    pub max_images_per_request: Option<u32>,\n    pub tool_calling: bool,\n    pub streaming: bool,\n}",
    "capability fields",
)
text = replace_once(
    text,
    "        let base_url = env::var(settings.base_url_env)\n            .unwrap_or_else(|_| settings.default_base_url.to_owned());\n        let client = shared_http_client()?;\n        Ok(Self {\n            client,\n            base_url: base_url.trim_end_matches('/').to_owned(),\n            api_key,\n            model: config.model.name.clone(),\n            capabilities: (settings.capabilities)(),\n        })",
    "        let base_url = config\n            .model\n            .base_url\n            .clone()\n            .or_else(|| env::var(settings.base_url_env).ok())\n            .unwrap_or_else(|| settings.default_base_url.to_owned());\n        let client = shared_http_client()?;\n        let mut capabilities = (settings.capabilities)();\n        capabilities.tool_calling = config.model.tool_calling;\n        capabilities.streaming = config.model.streaming;\n        Ok(Self {\n            client,\n            base_url: base_url.trim_end_matches('/').to_owned(),\n            api_key,\n            model: config.model.name.clone(),\n            capabilities,\n        })",
    "anthropic route resolution",
)
text = replace_once(
    text,
    "    fn validate_request(&self, request: &ModelRequest) -> MedusaResult<()> {\n        let images = request",
    "    fn validate_request(&self, request: &ModelRequest) -> MedusaResult<()> {\n        if !request.tools.is_empty() && !self.capabilities.tool_calling {\n            return Err(MedusaError::new(\n                ErrorCode::DependencyUnavailable,\n                ErrorCategory::Validation,\n                \"selected route does not support tool calling\",\n            ));\n        }\n        let images = request",
    "anthropic tool validation",
)
text = replace_once(
    text,
    "        ProviderCapabilities {\n            image_input: true,",
    "        ProviderCapabilities {\n            image_input: true,",
    "minimax capability anchor",
)
text = text.replace(
    "            max_images_per_request: Some(10),\n        }",
    "            max_images_per_request: Some(10),\n            tool_calling: true,\n            streaming: false,\n        }",
    1,
)
text = replace_once(
    text,
    "    } else {\n        ProviderCapabilities::default()\n    }\n}\n\nfn anthropic_capabilities()",
    "    } else {\n        ProviderCapabilities {\n            tool_calling: true,\n            streaming: false,\n            ..ProviderCapabilities::default()\n        }\n    }\n}\n\nfn anthropic_capabilities()",
    "minimax text capabilities",
)
text = text.replace(
    "        max_images_per_request: Some(20),\n    }",
    "        max_images_per_request: Some(20),\n        tool_calling: true,\n        streaming: false,\n    }",
    1,
)
old_manager = '''    /// Builds the configured primary provider plus ordered fallback providers.\n    pub fn manager_from_config(\n        config: &Config,\n        session_api_key: Option<String>,\n    ) -> MedusaResult<ProviderManager<Self>> {\n        let mut providers = vec![Self::from_config_with_api_key(\n            config,\n            session_api_key.clone(),\n        )?];\n        for fallback in &config.model.fallback_providers {\n            if fallback.eq_ignore_ascii_case(&config.model.provider) {\n                continue;\n            }\n            let mut fallback_config = config.clone();\n            fallback_config.model.provider = fallback.clone();\n            providers.push(Self::from_config_with_api_key(\n                &fallback_config,\n                session_api_key.clone(),\n            )?);\n        }\n        Ok(ProviderManager::new(providers))\n    }'''
new_manager = '''    /// Builds the configured primary provider plus ordered, self-contained fallback routes.\n    pub fn manager_from_config(\n        config: &Config,\n        session_api_key: Option<String>,\n    ) -> MedusaResult<ProviderManager<Self>> {\n        let mut providers = vec![Self::from_config_with_api_key(config, session_api_key)?];\n        let mut profiles = vec![route_profile(\n            \"primary\",\n            &config.model.provider,\n            &config.model.name,\n            &config.model.protocol,\n            config.model.base_url.as_deref(),\n            &config.model.auth,\n            config.model.tool_calling,\n            config.model.streaming,\n            config.model.max_retries,\n            config.model.retry_base_delay_ms,\n            config.model.retry_max_delay_ms,\n            config.model.retry_jitter_ms,\n        )];\n        for (index, fallback) in config.model.fallback_providers.iter().enumerate() {\n            let fallback_config = config_for_fallback(config, fallback);\n            providers.push(Self::from_config_with_api_key(&fallback_config, None).map_err(|mut error| {\n                error.context.insert(\"fallback_index\".to_owned(), Value::from(index as u64));\n                error.context.insert(\"provider\".to_owned(), Value::from(fallback.provider.clone()));\n                error.context.insert(\"model\".to_owned(), Value::from(fallback.name.clone()));\n                error\n            })?);\n            profiles.push(route_profile(\n                &format!(\"fallback[{index}]\"),\n                &fallback.provider,\n                &fallback.name,\n                &fallback.protocol,\n                fallback.base_url.as_deref(),\n                &fallback.auth,\n                fallback.tool_calling,\n                fallback.streaming,\n                fallback.max_retries,\n                fallback.retry_base_delay_ms,\n                fallback.retry_max_delay_ms,\n                fallback.retry_jitter_ms,\n            ));\n        }\n        Ok(ProviderManager::new_with_profiles(providers, profiles))\n    }'''
text = replace_once(text, old_manager, new_manager, "manager construction")
insert_before = "impl ModelProvider for ConfiguredProvider {"
helpers = '''fn config_for_fallback(config: &Config, fallback: &FallbackProviderConfig) -> Config {\n    let mut route = config.clone();\n    route.model.provider = fallback.provider.clone();\n    route.model.name = fallback.name.clone();\n    route.model.protocol = fallback.protocol.clone();\n    route.model.base_url = fallback.base_url.clone();\n    route.model.auth = fallback.auth.clone();\n    route.model.tool_calling = fallback.tool_calling;\n    route.model.streaming = fallback.streaming;\n    route.model.max_retries = fallback.max_retries;\n    route.model.retry_base_delay_ms = fallback.retry_base_delay_ms;\n    route.model.retry_max_delay_ms = fallback.retry_max_delay_ms;\n    route.model.retry_jitter_ms = fallback.retry_jitter_ms;\n    route.model.fallback_providers.clear();\n    route\n}\n\n#[allow(clippy::too_many_arguments)]\nfn route_profile(\n    id: &str,\n    provider: &str,\n    model: &str,\n    protocol: &str,\n    endpoint: Option<&str>,\n    auth: &str,\n    tool_calling: bool,\n    streaming: bool,\n    max_retries: u8,\n    base_delay_ms: u64,\n    max_delay_ms: u64,\n    jitter_ms: u64,\n) -> ProviderRouteProfile {\n    ProviderRouteProfile {\n        id: id.to_owned(),\n        provider: provider.to_owned(),\n        model: model.to_owned(),\n        protocol: protocol.to_owned(),\n        endpoint: endpoint.map(str::to_owned),\n        auth_source: auth.to_owned(),\n        tool_calling,\n        streaming,\n        retry: RouteRetryPolicy {\n            max_retries,\n            base_delay_ms,\n            max_delay_ms,\n            jitter_ms,\n        },\n    }\n}\n\n'''
text = replace_once(text, insert_before, helpers + insert_before, "route helpers")
text = replace_once(
    text,
    "pub struct OpenAiProvider {\n    client: Client,\n    base_url: String,\n    api_key: Option<String>,\n    model: String,\n}",
    "pub struct OpenAiProvider {\n    client: Client,\n    base_url: String,\n    api_key: Option<String>,\n    model: String,\n    capabilities: ProviderCapabilities,\n}",
    "openai capabilities field",
)
text = replace_once(
    text,
    "            api_key,\n            model: config.model.name.clone(),\n        })",
    "            api_key,\n            model: config.model.name.clone(),\n            capabilities: ProviderCapabilities {\n                tool_calling: config.model.tool_calling,\n                streaming: config.model.streaming,\n                ..ProviderCapabilities::default()\n            },\n        })",
    "openai capability init",
)
text = replace_once(
    text,
    "    fn request_body(&self, request: &ModelRequest) -> Value {\n        let mut messages",
    "    fn request_body(&self, request: &ModelRequest) -> Value {\n        let mut messages",
    "openai request anchor",
)
text = replace_once(
    text,
    "impl ModelProvider for OpenAiProvider {\n    fn complete(&self, request: &ModelRequest) -> MedusaResult<ModelResponse> {\n        let endpoint",
    "impl ModelProvider for OpenAiProvider {\n    fn complete(&self, request: &ModelRequest) -> MedusaResult<ModelResponse> {\n        if !request.tools.is_empty() && !self.capabilities.tool_calling {\n            return Err(MedusaError::new(\n                ErrorCode::DependencyUnavailable,\n                ErrorCategory::Validation,\n                \"selected route does not support tool calling\",\n            ));\n        }\n        let endpoint",
    "openai tool validation",
)
text = replace_once(
    text,
    "        Err(response_error(response))\n    }\n}\n\n#[derive(Debug, Deserialize)]\nstruct OpenAiWireResponse",
    "        Err(response_error(response))\n    }\n\n    fn capabilities(&self) -> ProviderCapabilities {\n        self.capabilities.clone()\n    }\n}\n\n#[derive(Debug, Deserialize)]\nstruct OpenAiWireResponse",
    "openai capabilities impl",
)
provider_path.write_text(text)

manager_path = Path("crates/medusa-provider/src/manager.rs")
text = manager_path.read_text()
text = replace_once(
    text,
    "#[derive(Clone, Copy, Debug, Eq, PartialEq)]\nstruct RetryPolicy {\n    max_retries_per_provider: u8,\n    base_delay_ms: u64,\n    max_delay_ms: u64,\n    jitter_ms: u64,\n}",
    "#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]\npub struct RouteRetryPolicy {\n    pub max_retries: u8,\n    pub base_delay_ms: u64,\n    pub max_delay_ms: u64,\n    pub jitter_ms: u64,\n}\n\n#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]\npub struct ProviderRouteProfile {\n    pub id: String,\n    pub provider: String,\n    pub model: String,\n    pub protocol: String,\n    pub endpoint: Option<String>,\n    pub auth_source: String,\n    pub tool_calling: bool,\n    pub streaming: bool,\n    pub retry: RouteRetryPolicy,\n}",
    "retry/profile structs",
)
text = text.replace("impl Default for RetryPolicy", "impl Default for RouteRetryPolicy", 1)
text = text.replace("max_retries_per_provider: 1,", "max_retries: 1,", 1)
text = text.replace("impl RetryPolicy {", "impl RouteRetryPolicy {", 1)
text = replace_once(
    text,
    "    policy: RetryPolicy,\n    cache:",
    "    profiles: Vec<ProviderRouteProfile>,\n    cache:",
    "manager profile field",
)
text = replace_once(
    text,
    "        Self {\n            providers,\n            policy: RetryPolicy::default(),",
    "        let profiles = (0..providers.len())\n            .map(|index| ProviderRouteProfile {\n                id: format!(\"provider[{index}]\"),\n                provider: format!(\"provider-{index}\"),\n                model: \"unspecified\".to_owned(),\n                protocol: \"unspecified\".to_owned(),\n                endpoint: None,\n                auth_source: \"unspecified\".to_owned(),\n                tool_calling: true,\n                streaming: false,\n                retry: RouteRetryPolicy::default(),\n            })\n            .collect();\n        Self {\n            providers,\n            profiles,",
    "manager defaults",
)
text = replace_once(
    text,
    "    #[must_use]\n    pub fn with_retries(mut self, retries_per_provider: u8) -> Self {\n        self.policy.max_retries_per_provider = retries_per_provider;\n        self\n    }",
    "    #[must_use]\n    pub fn new_with_profiles(\n        providers: Vec<P>,\n        mut profiles: Vec<ProviderRouteProfile>,\n    ) -> Self {\n        profiles.truncate(providers.len());\n        while profiles.len() < providers.len() {\n            let index = profiles.len();\n            profiles.push(ProviderRouteProfile {\n                id: format!(\"provider[{index}]\"),\n                provider: format!(\"provider-{index}\"),\n                model: \"unspecified\".to_owned(),\n                protocol: \"unspecified\".to_owned(),\n                endpoint: None,\n                auth_source: \"unspecified\".to_owned(),\n                tool_calling: true,\n                streaming: false,\n                retry: RouteRetryPolicy::default(),\n            });\n        }\n        let health = vec![ProviderHealth::default(); providers.len()];\n        Self {\n            providers,\n            profiles,\n            cache: Mutex::new(BTreeMap::new()),\n            health: Mutex::new(health),\n            last_completed_provider: Mutex::new(None),\n            cache_hits: Mutex::new(0),\n            last_execution: Mutex::new(None),\n            sleeper: thread::sleep,\n        }\n    }\n\n    #[must_use]\n    pub fn with_retries(mut self, retries_per_provider: u8) -> Self {\n        for profile in &mut self.profiles {\n            profile.retry.max_retries = retries_per_provider;\n        }\n        self\n    }",
    "profile constructor",
)
text = replace_once(
    text,
    "    fn with_policy(mut self, policy: RetryPolicy) -> Self {\n        self.policy = policy;\n        self\n    }",
    "    fn with_policy(mut self, policy: RouteRetryPolicy) -> Self {\n        for profile in &mut self.profiles {\n            profile.retry = policy;\n        }\n        self\n    }",
    "test policy",
)
text = replace_once(
    text,
    "        let snapshot = json!({\n            \"provider_index\": index,",
    "        let route = self.profiles.get(index);\n        let snapshot = json!({\n            \"provider_index\": index,\n            \"route_id\": route.map(|route| route.id.as_str()),\n            \"provider\": route.map(|route| route.provider.as_str()),\n            \"model\": route.map(|route| route.model.as_str()),\n            \"protocol\": route.map(|route| route.protocol.as_str()),\n            \"endpoint\": route.and_then(|route| route.endpoint.as_deref()),\n            \"auth_source\": route.map(|route| route.auth_source.as_str()),\n            \"tool_calling\": route.map(|route| route.tool_calling),\n            \"streaming\": route.map(|route| route.streaming),",
    "structured diagnostics",
)
text = replace_once(
    text,
    "        for (index, provider) in self.providers.iter().enumerate() {\n            let has_fallback = index + 1 < self.providers.len();\n            for attempt in 0..=self.policy.max_retries_per_provider {",
    "        for (index, provider) in self.providers.iter().enumerate() {\n            let has_fallback = index + 1 < self.providers.len();\n            let policy = self\n                .profiles\n                .get(index)\n                .map_or_else(RouteRetryPolicy::default, |profile| profile.retry);\n            for attempt in 0..=policy.max_retries {",
    "per-route retry loop",
)
text = text.replace("attempt < self.policy.max_retries_per_provider", "attempt < policy.max_retries", 1)
text = text.replace("let delay_ms = self.policy.delay_ms(&error, index, attempt);", "let delay_ms = policy.delay_ms(&error, index, attempt);", 1)
text = text.replace("RetryPolicy {", "RouteRetryPolicy {", 20)
text = text.replace("max_retries_per_provider:", "max_retries:")
manager_path.write_text(text)

print("issue 352 source migration applied")
