//! Shared runtime capability discovery for TUI, desktop, CLI, and agent prompts.

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use medusa_extensions::DesktopCommanderSettings;
use serde::{Deserialize, Serialize};

/// Stable capability keys emitted to every frontend and included in model context.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    Filesystem,
    Shell,
    Git,
    GitHub,
    Browser,
    DesktopMcp,
    Playwright,
    Memory,
    Network,
}

impl Capability {
    const ALL: [Self; 9] = [
        Self::Filesystem,
        Self::Shell,
        Self::Git,
        Self::GitHub,
        Self::Browser,
        Self::DesktopMcp,
        Self::Playwright,
        Self::Memory,
        Self::Network,
    ];

    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Filesystem => "Filesystem",
            Self::Shell => "Shell",
            Self::Git => "Git",
            Self::GitHub => "GitHub",
            Self::Browser => "Browser",
            Self::DesktopMcp => "Desktop MCP",
            Self::Playwright => "Playwright",
            Self::Memory => "Memory",
            Self::Network => "Network",
        }
    }
}

/// Live result of a capability probe, including why an unavailable tool was withheld.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CapabilityState {
    pub available: bool,
    pub detail: String,
}

/// One immutable discovery snapshot shared with all runtime consumers.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CapabilityRegistry {
    pub repository: PathBuf,
    pub capabilities: BTreeMap<Capability, CapabilityState>,
}

/// Command boundary used to keep platform capability detection deterministic in tests.
pub trait CommandProbe {
    fn available(&self, program: &str, arguments: &[&str]) -> bool;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SystemProbe;

impl CommandProbe for SystemProbe {
    fn available(&self, program: &str, arguments: &[&str]) -> bool {
        Command::new(program)
            .args(arguments)
            .output()
            .is_ok_and(|output| output.status.success())
    }
}

impl CapabilityRegistry {
    pub fn discover(repository: impl Into<PathBuf>) -> MedusaResult<Self> {
        Self::discover_with(repository.into(), &SystemProbe)
    }

    pub fn discover_with(repository: PathBuf, probe: &impl CommandProbe) -> MedusaResult<Self> {
        let mut capabilities = BTreeMap::new();
        let filesystem = repository.is_dir();
        insert(
            &mut capabilities,
            Capability::Filesystem,
            filesystem,
            if filesystem {
                "repository is accessible"
            } else {
                "repository path is unavailable"
            },
        );
        let shell = if cfg!(windows) {
            probe.available("cmd", &["/C", "echo", "medusa"])
        } else {
            probe.available("sh", &["-c", "true"])
        };
        insert(
            &mut capabilities,
            Capability::Shell,
            shell,
            if shell {
                "system shell is executable"
            } else {
                "no supported system shell"
            },
        );
        insert(
            &mut capabilities,
            Capability::Git,
            probe.available("git", &["--version"]),
            "git executable probe",
        );
        insert(
            &mut capabilities,
            Capability::GitHub,
            probe.available("gh", &["auth", "status"]),
            "GitHub CLI authentication probe",
        );
        let browser = medusa_config::BrowserConfig::default();
        let browser_ready =
            browser.enabled && browser.path.as_ref().is_some_and(|path| path.exists());
        insert(
            &mut capabilities,
            Capability::Browser,
            browser_ready,
            if browser_ready {
                "configured browser sidecar"
            } else {
                "browser sidecar is disabled or unavailable"
            },
        );
        let desktop = DesktopCommanderSettings::from_env();
        let desktop_enabled = desktop.enabled();
        insert(
            &mut capabilities,
            Capability::DesktopMcp,
            desktop_enabled,
            if desktop_enabled {
                "Desktop Commander MCP is enabled"
            } else {
                "Desktop Commander MCP is disabled"
            },
        );
        insert(
            &mut capabilities,
            Capability::Playwright,
            probe.available("node", &["--version"]),
            "Node.js is available for Playwright sidecar",
        );
        let memory = filesystem && writable_state_directory(&repository)?;
        insert(
            &mut capabilities,
            Capability::Memory,
            memory,
            if memory {
                "repository state directory is writable"
            } else {
                "repository state directory is unavailable or read-only"
            },
        );
        let network = !matches!(
            std::env::var("MEDUSA_NETWORK_DISABLED").as_deref(),
            Ok("1") | Ok("true")
        );
        insert(
            &mut capabilities,
            Capability::Network,
            network,
            if network {
                "network policy permits outbound connections"
            } else {
                "network disabled by MEDUSA_NETWORK_DISABLED"
            },
        );
        Ok(Self {
            repository,
            capabilities,
        })
    }

    #[must_use]
    pub fn state(&self, capability: Capability) -> CapabilityState {
        self.capabilities
            .get(&capability)
            .cloned()
            .unwrap_or(CapabilityState {
                available: false,
                detail: "capability was not discovered".into(),
            })
    }

    #[must_use]
    pub fn available(&self, capability: Capability) -> bool {
        self.state(capability).available
    }

    /// Compact prompt-safe capability matrix; frontends should render this exact shared truth.
    #[must_use]
    pub fn prompt_summary(&self) -> String {
        Capability::ALL
            .into_iter()
            .map(|capability| {
                let marker = if self.available(capability) {
                    "✓"
                } else {
                    "✗"
                };
                format!("{} {marker}", capability.label())
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

fn insert(
    registry: &mut BTreeMap<Capability, CapabilityState>,
    capability: Capability,
    available: bool,
    detail: &str,
) {
    registry.insert(
        capability,
        CapabilityState {
            available,
            detail: detail.to_owned(),
        },
    );
}

fn writable_state_directory(repository: &Path) -> MedusaResult<bool> {
    let directory = repository.join(".medusa");
    fs::create_dir_all(&directory).map_err(io_error)?;
    let probe = directory.join(".capability-probe");
    match fs::write(&probe, b"probe") {
        Ok(()) => {
            fs::remove_file(probe).map_err(io_error)?;
            Ok(true)
        }
        Err(_) => Ok(false),
    }
}

fn io_error(error: std::io::Error) -> MedusaError {
    MedusaError::new(
        ErrorCode::PersistenceFailed,
        ErrorCategory::Environment,
        error.to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    struct FakeProbe(BTreeSet<String>);
    impl CommandProbe for FakeProbe {
        fn available(&self, program: &str, _: &[&str]) -> bool {
            self.0.contains(program)
        }
    }

    #[test]
    fn one_registry_reports_a_prompt_safe_shared_matrix() {
        let directory = tempfile::tempdir().expect("tempdir");
        let probe = FakeProbe(BTreeSet::from([
            "git".into(),
            "gh".into(),
            "node".into(),
            if cfg!(windows) {
                "cmd".into()
            } else {
                "sh".into()
            },
        ]));
        let registry =
            CapabilityRegistry::discover_with(directory.path().into(), &probe).expect("discover");
        assert!(registry.available(Capability::Filesystem));
        assert!(registry.available(Capability::GitHub));
        assert!(registry.available(Capability::Memory));
        let prompt = registry.prompt_summary();
        assert!(prompt.contains("Filesystem ✓"));
        assert!(prompt.contains("GitHub ✓"));
        assert_eq!(prompt.lines().count(), 9);
    }

    #[test]
    fn unavailable_tools_remain_visible_with_an_explanation() {
        let directory = tempfile::tempdir().expect("tempdir");
        let registry =
            CapabilityRegistry::discover_with(directory.path().into(), &FakeProbe(BTreeSet::new()))
                .expect("discover");
        assert!(!registry.available(Capability::Git));
        assert!(registry.state(Capability::Git).detail.contains("probe"));
        assert!(registry.prompt_summary().contains("Git ✗"));
    }
}
