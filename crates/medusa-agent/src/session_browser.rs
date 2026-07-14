use std::path::{Path, PathBuf};
use std::time::Duration;

use medusa_browser_client::BrowserClient;
use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};

#[derive(Clone, Debug)]
pub struct SessionBrowserConfig {
    pub enabled: bool,
    pub path: Option<PathBuf>,
    pub timeout: Duration,
}

impl Default for SessionBrowserConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            path: None,
            timeout: Duration::from_secs(30),
        }
    }
}

pub struct SessionBrowser {
    #[allow(dead_code)]
    config: SessionBrowserConfig,
    client: Option<BrowserClient>,
}

impl SessionBrowser {
    pub fn connect(config: &SessionBrowserConfig) -> MedusaResult<Self> {
        if !config.enabled {
            return Ok(Self {
                config: config.clone(),
                client: None,
            });
        }
        let path = resolve_path(config.path.as_deref())?;
        if !path.exists() {
            return Ok(Self {
                config: config.clone(),
                client: None,
            });
        }
        let client = BrowserClient::spawn(path.to_str().ok_or_else(|| invalid("non-utf8 path"))?)?;
        Ok(Self {
            config: config.clone(),
            client: Some(client),
        })
    }

    #[must_use]
    pub fn is_enabled(&self) -> bool {
        self.client.is_some()
    }

    pub fn client(&mut self) -> MedusaResult<&mut BrowserClient> {
        self.client
            .as_mut()
            .ok_or_else(|| unavailable("browser is not enabled in this session"))
    }
}

fn resolve_path(configured: Option<&Path>) -> MedusaResult<PathBuf> {
    if let Some(path) = configured {
        return Ok(path.to_path_buf());
    }
    let exe_name = if cfg!(windows) {
        "medusa-browserd.exe"
    } else {
        "medusa-browserd"
    };
    let agent_exe = std::env::current_exe()
        .map_err(|e| unavailable(format!("current_exe: {e}")))?;
    let adjacent = agent_exe.parent().map(|p| p.join(exe_name));
    if let Some(adj) = &adjacent {
        if adj.exists() {
            return Ok(adj.clone());
        }
    }
    if let Ok(found) = which(exe_name) {
        return Ok(found);
    }
    Err(unavailable(format!(
        "{exe_name} not found on PATH and not adjacent to the agent binary"
    )))
}

fn which(cmd: &str) -> Result<PathBuf, ()> {
    let path = std::env::var_os("PATH").ok_or(())?;
    for entry in std::env::split_paths(&path) {
        let candidate = entry.join(cmd);
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    Err(())
}

fn unavailable(message: impl Into<String>) -> MedusaError {
    MedusaError::new(ErrorCode::DependencyUnavailable, ErrorCategory::Transient, message)
        .with_retryable(true)
}

fn invalid(message: &'static str) -> MedusaError {
    MedusaError::new(
        ErrorCode::InvalidConfiguration,
        ErrorCategory::Validation,
        message,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_browser_disabled_when_path_missing() {
        let config = SessionBrowserConfig {
            enabled: true,
            path: Some(std::path::PathBuf::from("/nonexistent/medusa-browserd")),
            timeout: std::time::Duration::from_secs(5),
        };
        let session = SessionBrowser::connect(&config).unwrap();
        assert!(!session.is_enabled());
    }

    #[test]
    fn session_browser_disabled_when_flag_false() {
        let config = SessionBrowserConfig {
            enabled: false,
            path: None,
            timeout: std::time::Duration::from_secs(5),
        };
        let session = SessionBrowser::connect(&config).unwrap();
        assert!(!session.is_enabled());
    }
}