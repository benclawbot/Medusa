use clap::Parser;
use medusa_config::Config;
use serde::Serialize;
use std::{collections::BTreeMap, fs, path::PathBuf};

#[derive(Debug, Parser)]
#[command(
    name = "medusa-provider-diagnostic",
    about = "Validate a Medusa provider route before coding begins"
)]
struct Args {
    /// Optional Medusa TOML configuration to validate instead of the resolved default profile.
    #[arg(long)]
    config: Option<PathBuf>,
    /// Write the machine-readable report to this path as well as stdout.
    #[arg(long)]
    output: Option<PathBuf>,
}

#[derive(Debug, Serialize)]
struct DiagnosticReport {
    schema_version: u16,
    status: &'static str,
    provider: String,
    model: String,
    protocol: String,
    authentication: Capability,
    model_availability: Capability,
    minimal_completion: Capability,
    tool_use: Capability,
    image_input: Capability,
    context_window: Capability,
    streaming: Capability,
    external_dependencies: Vec<String>,
    failures: Vec<String>,
}

#[derive(Debug, Serialize)]
struct Capability {
    supported: bool,
    detail: String,
}

fn main() {
    match run() {
        Ok(true) => {}
        Ok(false) => std::process::exit(1),
        Err(error) => {
            eprintln!("provider diagnostic failed: {error}");
            std::process::exit(2);
        }
    }
}

fn run() -> Result<bool, Box<dyn std::error::Error>> {
    let args = Args::parse();
    let config = match args.config.as_deref() {
        Some(path) => Config::from_toml(&fs::read_to_string(path)?)?,
        None => Config::load_layers(None, None, &BTreeMap::new(), &BTreeMap::new())?,
    };
    let report = diagnose(&config);
    let json = serde_json::to_vec_pretty(&report)?;
    println!("{}", String::from_utf8_lossy(&json));
    if let Some(path) = args.output {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, &json)?;
    }
    Ok(report.failures.is_empty())
}

fn diagnose(config: &Config) -> DiagnosticReport {
    let route = &config.model;
    let provider = route.provider.trim().to_ascii_lowercase();
    let protocol = route.protocol.trim().to_ascii_lowercase();
    let mut failures = Vec::new();
    let mut external_dependencies = Vec::new();

    let supported_provider = matches!(
        provider.as_str(),
        "openai" | "openai-oauth" | "anthropic" | "minimax" | "local" | "custom"
    );
    if !supported_provider {
        failures.push(format!(
            "provider `{}` is not in the supported first-run set; use ChatGPT/OpenAI, Anthropic, local, or an explicit custom endpoint",
            route.provider
        ));
    }

    let auth_supported = matches!(route.auth.as_str(), "api-key" | "none");
    if !auth_supported {
        failures.push(format!(
            "authentication mode `{}` is unsupported",
            route.auth
        ));
    }

    if route.auth == "api-key" && !credential_present(&provider) {
        failures.push(format!(
            "no credential found for provider `{}`; configure the provider-specific API key environment variable",
            route.provider
        ));
    }

    if matches!(provider.as_str(), "openai-oauth") {
        external_dependencies.push("Codex CLI app-server (codex app-server --stdio)".into());
    }
    if provider == "local" {
        external_dependencies.push(
            route
                .base_url
                .clone()
                .unwrap_or_else(|| "local OpenAI-compatible runtime".into()),
        );
    }
    if provider == "custom" && route.base_url.is_none() {
        failures.push("custom provider routes require an explicit base_url".into());
    }

    let protocol_supported = matches!(protocol.as_str(), "openai" | "anthropic");
    if !protocol_supported {
        failures.push(format!("protocol `{}` is unsupported", route.protocol));
    }

    if route.streaming {
        failures.push(
            "streaming is configured but the production adapter contract does not guarantee native streaming for this route"
                .into(),
        );
    }

    let model_present = !route.name.trim().is_empty();
    if !model_present {
        failures.push("model name is empty".into());
    }

    DiagnosticReport {
        schema_version: 1,
        status: if failures.is_empty() {
            "ready"
        } else {
            "blocked"
        },
        provider: route.provider.clone(),
        model: route.name.clone(),
        protocol: route.protocol.clone(),
        authentication: Capability {
            supported: auth_supported && (route.auth != "api-key" || credential_present(&provider)),
            detail: if route.auth == "api-key" {
                "credential is read from the environment and is never persisted in the report".into()
            } else {
                format!("authentication mode: {}", route.auth)
            },
        },
        model_availability: Capability {
            supported: model_present && supported_provider,
            detail: "configuration-level validation only; live availability requires an optional credentialed canary"
                .into(),
        },
        minimal_completion: Capability {
            supported: model_present && supported_provider && protocol_supported,
            detail: "deterministic route compatibility passed; live completion is intentionally separate"
                .into(),
        },
        tool_use: Capability {
            supported: route.tool_calling && protocol_supported,
            detail: if route.tool_calling {
                "tool calling is enabled for the configured protocol".into()
            } else {
                "tool calling is disabled; coding sessions are blocked from claiming tool support".into()
            },
        },
        image_input: Capability {
            supported: false,
            detail: "image input is not part of the stable provider setup contract".into(),
        },
        context_window: Capability {
            supported: route.context_window_tokens > 0,
            detail: format!(
                "configured context window: {} tokens",
                route.context_window_tokens
            ),
        },
        streaming: Capability {
            supported: !route.streaming,
            detail: if route.streaming {
                "unsupported advertised streaming was rejected".into()
            } else {
                "streaming is not advertised for this route".into()
            },
        },
        external_dependencies,
        failures,
    }
}

fn credential_present(provider: &str) -> bool {
    let provider_key = format!(
        "{}_API_KEY",
        provider.to_ascii_uppercase().replace('-', "_")
    );
    std::env::var_os(provider_key).is_some()
        || std::env::var_os("OPENAI_API_KEY").is_some()
        || std::env::var_os("ANTHROPIC_API_KEY").is_some()
        || std::env::var_os("MINIMAX_API_KEY").is_some()
        || std::env::var_os("MEDUSA_API_KEY").is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unsupported_streaming_fails_closed() {
        let mut config = Config::default();
        config.model.auth = "none".into();
        config.model.streaming = true;
        let report = diagnose(&config);
        assert_eq!(report.status, "blocked");
        assert!(
            report
                .failures
                .iter()
                .any(|failure| failure.contains("streaming"))
        );
    }

    #[test]
    fn local_route_reports_dependency_without_secrets() {
        let mut config = Config::default();
        config.model.provider = "local".into();
        config.model.protocol = "openai".into();
        config.model.auth = "none".into();
        config.model.base_url = Some("http://127.0.0.1:11434/v1".into());
        config.model.streaming = false;
        let report = diagnose(&config);
        assert_eq!(report.status, "ready");
        assert_eq!(report.external_dependencies.len(), 1);
    }
}
