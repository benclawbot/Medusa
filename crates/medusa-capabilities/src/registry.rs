use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use medusa_browser_client::verification_route::VerificationRoute;
use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use medusa_extensions::DesktopCommanderSettings;
use medusa_provider::ToolDefinition;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub const CAPABILITY_REGISTRY_SCHEMA_VERSION: u16 = 3;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    Filesystem,
    CodeIntelligence,
    Shell,
    Git,
    GitHub,
    SelfImprovement,
    Browser,
    DesktopMcp,
    Playwright,
    Memory,
    Network,
}

impl Capability {
    pub const ALL: [Self; 11] = [
        Self::Filesystem,
        Self::CodeIntelligence,
        Self::Shell,
        Self::Git,
        Self::GitHub,
        Self::SelfImprovement,
        Self::Browser,
        Self::DesktopMcp,
        Self::Playwright,
        Self::Memory,
        Self::Network,
    ];

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Filesystem => "Filesystem",
            Self::CodeIntelligence => "Code intelligence",
            Self::Shell => "Shell",
            Self::Git => "Git",
            Self::GitHub => "GitHub",
            Self::SelfImprovement => "Self-improvement",
            Self::Browser => "Browser",
            Self::DesktopMcp => "Desktop MCP",
            Self::Playwright => "Playwright",
            Self::Memory => "Memory",
            Self::Network => "Network",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RegistryKind {
    Capability,
    Tool,
    Command,
    Provider,
    Plugin,
    Frontend,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum CapabilityStatus {
    DesignOnly,
    Experimental,
    Partial,
    Production,
    Deprecated,
    Deleted,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilitySurface {
    Model,
    Cli,
    Ui,
    Protocol,
    Documentation,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RegistryPermission {
    Read,
    Write,
    Network,
    RepositoryMutation,
    RuntimeMutation,
    CredentialUse,
    ProcessSpawn,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilityState {
    pub available: bool,
    pub detail: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReadinessContract {
    pub ready: bool,
    pub detail: String,
    pub evidence: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HandlerRegistration {
    pub id: String,
    pub lifecycle_owner: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RegistryEntry {
    pub id: String,
    pub capability: Capability,
    pub kind: RegistryKind,
    pub status: CapabilityStatus,
    pub description: String,
    pub owner: String,
    pub lifecycle_owner: String,
    pub surfaces: BTreeSet<CapabilitySurface>,
    pub permissions: BTreeSet<RegistryPermission>,
    pub explicit_approval: BTreeSet<RegistryPermission>,
    pub dependencies: Vec<String>,
    pub supported_platforms: BTreeSet<String>,
    pub readiness: ReadinessContract,
    pub handler: Option<HandlerRegistration>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<ToolDefinition>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<String>,
}

impl RegistryEntry {
    #[must_use]
    pub fn projected_to(&self, surface: CapabilitySurface) -> bool {
        self.surfaces.contains(&surface)
            && self.readiness.ready
            && !matches!(
                self.status,
                CapabilityStatus::DesignOnly
                    | CapabilityStatus::Deprecated
                    | CapabilityStatus::Deleted
            )
    }

    fn validate(&self) -> MedusaResult<()> {
        if self.id.trim().is_empty()
            || self.description.trim().is_empty()
            || self.owner.trim().is_empty()
            || self.lifecycle_owner.trim().is_empty()
            || self.readiness.detail.trim().is_empty()
            || self
                .readiness
                .evidence
                .iter()
                .any(|item| item.trim().is_empty())
        {
            return Err(invalid("capability registry entry is incomplete"));
        }
        if !valid_id(&self.id) {
            return Err(invalid(format!(
                "capability registry id must be lowercase dotted or kebab-case: {}",
                self.id
            )));
        }
        if !self.explicit_approval.is_subset(&self.permissions) {
            return Err(invalid(format!(
                "{} requires approval for undeclared permissions",
                self.id
            )));
        }
        if self.surfaces.contains(&CapabilitySurface::Model) {
            if self.tool.is_none() {
                return Err(invalid(format!(
                    "{} projects to the model without a tool contract",
                    self.id
                )));
            }
            if self.handler.is_none() {
                return Err(invalid(format!(
                    "{} projects to the model without a production handler",
                    self.id
                )));
            }
        }
        if self.tool.is_some() && self.kind != RegistryKind::Tool {
            return Err(invalid(format!(
                "{} has a tool contract but is not a tool entry",
                self.id
            )));
        }
        if let Some(handler) = &self.handler
            && (handler.id.trim().is_empty() || handler.lifecycle_owner.trim().is_empty())
        {
            return Err(invalid(format!("{} has an incomplete handler", self.id)));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RegistryReport {
    pub schema_version: u16,
    pub repository: PathBuf,
    pub entries: Vec<RegistryEntry>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilityRegistry {
    pub repository: PathBuf,
    pub capabilities: BTreeMap<Capability, CapabilityState>,
    pub entries: BTreeMap<String, RegistryEntry>,
}

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
        Self::discover_with_desktop(
            repository.into(),
            &SystemProbe,
            DesktopCommanderSettings::from_env(),
        )
    }

    pub fn discover_with(repository: PathBuf, probe: &impl CommandProbe) -> MedusaResult<Self> {
        Self::discover_with_desktop(repository, probe, DesktopCommanderSettings::from_env())
    }

    pub fn discover_with_desktop(
        repository: PathBuf,
        probe: &impl CommandProbe,
        desktop: DesktopCommanderSettings,
    ) -> MedusaResult<Self> {
        let mut capabilities = BTreeMap::new();
        let filesystem = repository.is_dir();
        insert_state(
            &mut capabilities,
            Capability::Filesystem,
            filesystem,
            if filesystem {
                "repository is accessible"
            } else {
                "repository path is unavailable"
            },
        );
        insert_state(
            &mut capabilities,
            Capability::CodeIntelligence,
            filesystem,
            if filesystem {
                "Rust/Python indexing and repository-scoped TypeScript/JavaScript semantic and guarded-refactoring tools are available; exact per-language levels are reported by semantic_capabilities"
            } else {
                "code intelligence requires an accessible repository"
            },
        );
        let shell = if cfg!(windows) {
            probe.available("cmd", &["/C", "echo", "medusa"])
        } else {
            probe.available("sh", &["-c", "true"])
        };
        insert_state(
            &mut capabilities,
            Capability::Shell,
            shell,
            if shell {
                "system shell is executable"
            } else {
                "no supported system shell"
            },
        );
        insert_state(
            &mut capabilities,
            Capability::Git,
            probe.available("git", &["--version"]),
            "git executable probe",
        );
        insert_state(
            &mut capabilities,
            Capability::GitHub,
            probe.available("gh", &["auth", "status"]),
            "GitHub CLI authentication probe",
        );
        let browser = medusa_config::BrowserConfig::default();
        let verification_route = std::env::var("MEDUSA_BROWSER_VERIFY_URL").ok();
        capabilities.insert(
            Capability::Browser,
            browser_capability_state(&browser, verification_route.as_deref(), probe),
        );
        let desktop_enabled = desktop.enabled();
        insert_state(
            &mut capabilities,
            Capability::DesktopMcp,
            desktop_enabled,
            if desktop_enabled {
                "Desktop Commander MCP is enabled"
            } else {
                "Desktop Commander MCP is disabled"
            },
        );
        insert_state(
            &mut capabilities,
            Capability::Playwright,
            probe.available("node", &["--version"]),
            "Node.js executable probe",
        );
        let memory = filesystem && writable_state_directory(&repository)?;
        insert_state(
            &mut capabilities,
            Capability::Memory,
            memory,
            if memory {
                "repository state directory is writable"
            } else {
                "repository state directory is unavailable or read-only"
            },
        );
        insert_state(
            &mut capabilities,
            Capability::SelfImprovement,
            memory,
            if memory {
                "self-improvement proposals can be staged for review"
            } else {
                "self-improvement requires writable repository state"
            },
        );
        let network = !matches!(
            std::env::var("MEDUSA_NETWORK_DISABLED").as_deref(),
            Ok("1") | Ok("true")
        );
        insert_state(
            &mut capabilities,
            Capability::Network,
            network,
            if network {
                "network policy permits outbound connections"
            } else {
                "network disabled by MEDUSA_NETWORK_DISABLED"
            },
        );

        let entries = build_entries(&capabilities, &desktop);
        let registry = Self {
            repository,
            capabilities,
            entries,
        };
        registry.validate()?;
        Ok(registry)
    }

    pub fn from_entries(
        repository: PathBuf,
        capabilities: BTreeMap<Capability, CapabilityState>,
        entries: Vec<RegistryEntry>,
    ) -> MedusaResult<Self> {
        let mut indexed = BTreeMap::new();
        for entry in entries {
            if indexed.insert(entry.id.clone(), entry).is_some() {
                return Err(invalid("duplicate capability registry id"));
            }
        }
        let registry = Self {
            repository,
            capabilities,
            entries: indexed,
        };
        registry.validate()?;
        Ok(registry)
    }

    pub fn validate(&self) -> MedusaResult<()> {
        let mut tool_names = BTreeSet::new();
        let mut handler_ids = BTreeSet::new();
        for (id, entry) in &self.entries {
            if id != &entry.id {
                return Err(invalid("capability registry index does not match entry id"));
            }
            entry.validate()?;
            if let Some(tool) = &entry.tool
                && !tool_names.insert(tool.name.as_str())
            {
                return Err(invalid(format!("duplicate model tool name: {}", tool.name)));
            }
            if let Some(handler) = &entry.handler
                && !handler_ids.insert(handler.id.as_str())
            {
                return Err(invalid(format!(
                    "duplicate production handler id: {}",
                    handler.id
                )));
            }
        }
        Ok(())
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

    #[must_use]
    pub fn entry(&self, id: &str) -> Option<&RegistryEntry> {
        self.entries.get(id)
    }

    #[must_use]
    pub fn model_tools(&self, read_only: bool) -> Vec<ToolDefinition> {
        self.entries
            .values()
            .filter(|entry| entry.projected_to(CapabilitySurface::Model))
            .filter(|entry| !read_only || entry.capability != Capability::Browser)
            .filter(|entry| !read_only || !mutating(entry))
            .filter_map(|entry| entry.tool.clone())
            .collect()
    }

    #[must_use]
    pub fn protocol_report(&self) -> RegistryReport {
        RegistryReport {
            schema_version: CAPABILITY_REGISTRY_SCHEMA_VERSION,
            repository: self.repository.clone(),
            entries: self.entries.values().cloned().collect(),
        }
    }

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

fn browser_capability_state(
    browser: &medusa_config::BrowserConfig,
    verification_route: Option<&str>,
    probe: &impl CommandProbe,
) -> CapabilityState {
    if !browser.enabled {
        return CapabilityState {
            available: false,
            detail: "browser model actions are explicitly disabled".into(),
        };
    }
    let Some(path) = browser.path.as_ref() else {
        return CapabilityState {
            available: false,
            detail: "browser model actions require an explicit MEDUSA_BROWSER_PATH sidecar".into(),
        };
    };
    let Some(program) = path.to_str() else {
        return CapabilityState {
            available: false,
            detail: "browser sidecar path is not UTF-8".into(),
        };
    };
    let Some(raw_route) = verification_route else {
        return CapabilityState {
            available: false,
            detail: "browser model actions require a Medusa-owned verification route".into(),
        };
    };
    let route = match VerificationRoute::parse(raw_route) {
        Ok(route) => route,
        Err(error) => {
            return CapabilityState {
                available: false,
                detail: format!("invalid browser verification route: {error}"),
            };
        }
    };
    if !probe.available(program, &["--check"]) {
        return CapabilityState {
            available: false,
            detail: "configured browser sidecar failed its readiness probe".into(),
        };
    }
    if !probe.available("node", &["--version"]) {
        return CapabilityState {
            available: false,
            detail: "browser sidecar requires Node.js for the Playwright bridge".into(),
        };
    }
    CapabilityState {
        available: true,
        detail: format!(
            "browser sidecar is bound to an admitted Medusa-owned verification route ({})",
            route.safe_fingerprint()
        ),
    }
}

fn build_entries(
    states: &BTreeMap<Capability, CapabilityState>,
    desktop: &DesktopCommanderSettings,
) -> BTreeMap<String, RegistryEntry> {
    let mut entries = BTreeMap::new();
    for entry in builtin_tool_entries(states, desktop) {
        entries.insert(entry.id.clone(), entry);
    }
    for name in [
        "browser_navigate",
        "browser_snapshot",
        "browser_click",
        "browser_fill",
        "browser_press",
        "browser_screenshot",
        "browser_tabs",
        "browser_close",
        "browser_ping",
    ] {
        let (description, input_schema) = match name {
            "browser_navigate" => (
                "Navigate the isolated verification browser to Medusa's configured verification route.",
                json!({"type":"object","properties":{},"additionalProperties":false}),
            ),
            "browser_snapshot" => (
                "Read a bounded text/accessibility snapshot from the isolated verification browser.",
                json!({"type":"object","properties":{},"additionalProperties":false}),
            ),
            "browser_click" => (
                "Click one element in the isolated verification browser by snapshot ref or selector.",
                json!({"type":"object","properties":{"ref":{"type":"integer","minimum":0},"selector":{"type":"string"}},"additionalProperties":false}),
            ),
            "browser_fill" => (
                "Fill one control in the isolated verification browser by snapshot ref or selector.",
                json!({"type":"object","properties":{"ref":{"type":"integer","minimum":0},"selector":{"type":"string"},"value":{"type":"string"}},"required":["value"],"additionalProperties":false}),
            ),
            "browser_press" => (
                "Press one keyboard key in the isolated verification browser.",
                json!({"type":"object","properties":{"key":{"type":"string"}},"required":["key"],"additionalProperties":false}),
            ),
            "browser_screenshot" => (
                "Capture a screenshot artifact from the isolated verification browser.",
                json!({"type":"object","properties":{"full_page":{"type":"boolean"}},"additionalProperties":false}),
            ),
            "browser_tabs" => (
                "List tabs in the isolated verification browser.",
                json!({"type":"object","properties":{},"additionalProperties":false}),
            ),
            "browser_close" => (
                "Close and forget the repository-scoped isolated verification browser.",
                json!({"type":"object","properties":{},"additionalProperties":false}),
            ),
            "browser_ping" => (
                "Check that the isolated verification browser sidecar is responsive.",
                json!({"type":"object","properties":{},"additionalProperties":false}),
            ),
            _ => continue,
        };
        let handler = format!("medusa-agent::tools::browser::{name}");
        let mut entry = tool_entry(
            ToolIdentity { states, name },
            Capability::Browser,
            description,
            input_schema,
            &handler,
            [
                RegistryPermission::Read,
                RegistryPermission::Network,
                RegistryPermission::ProcessSpawn,
                RegistryPermission::RuntimeMutation,
            ],
            false,
        );
        entry.status = CapabilityStatus::Production;
        entry.provenance = Some("verified-browser production".into());
        entries.insert(entry.id.clone(), entry);
    }
    let evaluate = RegistryEntry {
        id: "tool.browser_evaluate".into(),
        capability: Capability::Browser,
        kind: RegistryKind::Tool,
        status: CapabilityStatus::Partial,
        description:
            "Arbitrary JavaScript evaluation is reserved for Medusa's authoritative verifier".into(),
        owner: "browser maintainers".into(),
        lifecycle_owner: "medusa-browserd".into(),
        surfaces: BTreeSet::from([
            CapabilitySurface::Protocol,
            CapabilitySurface::Documentation,
        ]),
        permissions: BTreeSet::from([RegistryPermission::Read, RegistryPermission::Network]),
        explicit_approval: BTreeSet::new(),
        dependencies: vec!["browser sidecar".into()],
        supported_platforms: supported_platforms(),
        readiness: ReadinessContract {
            ready: false,
            detail: "browser_evaluate is verifier-internal and never model-executable".into(),
            evidence: vec!["authoritative verifier owns JavaScript evaluation".into()],
        },
        handler: None,
        tool: None,
        provenance: Some("verified-browser least-privilege boundary".into()),
    };
    entries.insert(evaluate.id.clone(), evaluate);
    entries
}

fn builtin_tool_entries(
    states: &BTreeMap<Capability, CapabilityState>,
    desktop: &DesktopCommanderSettings,
) -> Vec<RegistryEntry> {
    let mut entries = vec![
        tool_entry(
            ToolIdentity {
                states,
                name: "fs_read",
            },
            Capability::Filesystem,
            "Read a UTF-8 file inside the repository. Use path `.` to list repository files.",
            json!({"type":"object","properties":{"path":{"type":"string"}},"required":["path"],"additionalProperties":false}),
            "medusa-agent::tools::filesystem::read",
            [RegistryPermission::Read],
            false,
        ),
        tool_entry(
            ToolIdentity {
                states,
                name: "fs_create_dir",
            },
            Capability::Filesystem,
            "Create a directory and missing parents inside the repository.",
            json!({"type":"object","properties":{"path":{"type":"string"}},"required":["path"],"additionalProperties":false}),
            "medusa-agent::tools::filesystem::create_dir",
            [
                RegistryPermission::Write,
                RegistryPermission::RepositoryMutation,
            ],
            true,
        ),
        tool_entry(
            ToolIdentity {
                states,
                name: "fs_write",
            },
            Capability::Filesystem,
            "Atomically write a UTF-8 file inside the repository.",
            json!({"type":"object","properties":{"path":{"type":"string"},"content":{"type":"string"}},"required":["path","content"],"additionalProperties":false}),
            "medusa-agent::tools::filesystem::write",
            [
                RegistryPermission::Write,
                RegistryPermission::RepositoryMutation,
            ],
            true,
        ),
        tool_entry(
            ToolIdentity {
                states,
                name: "search_text",
            },
            Capability::Filesystem,
            "Search UTF-8 repository files for an exact text fragment.",
            json!({"type":"object","properties":{"query":{"type":"string"}},"required":["query"],"additionalProperties":false}),
            "medusa-agent::tools::filesystem::search",
            [RegistryPermission::Read],
            false,
        ),
        tool_entry(
            ToolIdentity {
                states,
                name: "semantic_capabilities",
            },
            Capability::CodeIntelligence,
            "Report exact production, partial, and unavailable semantic capability levels for Rust, Python, and TypeScript/JavaScript.",
            json!({"type":"object","properties":{},"additionalProperties":false}),
            "medusa-agent::tools::intelligence::semantic_capabilities",
            [RegistryPermission::Read],
            false,
        ),
        tool_entry(
            ToolIdentity {
                states,
                name: "code_index",
            },
            Capability::CodeIntelligence,
            "Build the Rust tree-sitter and Python lexical symbol/reference index and optionally query one exact identifier.",
            json!({"type":"object","properties":{"name":{"type":"string"}},"additionalProperties":false}),
            "medusa-agent::tools::intelligence::code_index",
            [RegistryPermission::Read],
            false,
        ),
        tool_entry(
            ToolIdentity {
                states,
                name: "inspect_target",
            },
            Capability::CodeIntelligence,
            "Inspect one path or symbol with a bounded revision-bound evidence packet containing definitions, references, source excerpts, ownership metadata, dependency impact, related verification, repository instructions, freshness, confidence, omissions, and evidence IDs. Prefer this compound tool over chains of fs_read/search_text/code_index calls.",
            json!({"type":"object","properties":{"path":{"type":"string"},"symbol":{"type":"string"}},"oneOf":[{"required":["path"]},{"required":["symbol"]}],"additionalProperties":false}),
            "medusa-agent::tools::compound::inspect_target",
            [RegistryPermission::Read],
            false,
        ),
        tool_entry(
            ToolIdentity {
                states,
                name: "apply_structured_patch",
            },
            Capability::CodeIntelligence,
            "Apply one revision-bound atomic structured repository mutation. Supports guarded byte-range updates plus create/delete/rename/move operations, requires exact repository revision and content hashes for existing files, rejects overlapping/stale/unsafe paths, and returns transaction hashes, public-API risk, invalidated graph nodes, and stable evidence references. Prefer this over whole-file rewrites when the target is known.",
            json!({"type":"object","properties":{"repository_revision":{"type":"string"},"plan_id":{"type":"string"},"edits":{"type":"array","items":{"type":"object","properties":{"path":{"type":"string"},"file_hash":{"type":"string"},"start_byte":{"type":"integer","minimum":0},"end_byte":{"type":"integer","minimum":0},"expected_content":{"type":"string"},"replacement":{"type":"string"},"intent":{"type":"string"},"annotation":{"type":"string"}},"required":["path","file_hash","start_byte","end_byte","replacement"],"additionalProperties":false}},"file_operations":{"type":"array","items":{"type":"object","properties":{"kind":{"type":"string","enum":["create","delete","rename","move"]},"path":{"type":"string"},"from":{"type":"string"},"to":{"type":"string"},"content":{"type":"string"},"expected_hash":{"type":"string"},"overwrite":{"type":"boolean"},"intent":{"type":"string"},"annotation":{"type":"string"}},"required":["kind"],"additionalProperties":false}}},"required":["repository_revision","plan_id"],"additionalProperties":false}),
            "medusa-agent::tools::compound::apply_structured_patch",
            [
                RegistryPermission::Write,
                RegistryPermission::RepositoryMutation,
            ],
            true,
        ),
        tool_entry(
            ToolIdentity {
                states,
                name: "verify_impacted",
            },
            Capability::CodeIntelligence,
            "Run the smallest conservative authoritative verification set for exact revision-bound changed paths, using repository-graph test impact and returning structured pass/failure evidence and escalation requirements. Prefer this over manually chaining shell verification commands.",
            json!({"type":"object","properties":{"repository_revision":{"type":"string"},"paths":{"type":"array","minItems":1,"items":{"type":"string"}}},"required":["repository_revision","paths"],"additionalProperties":false}),
            "medusa-agent::tools::compound::verify_impacted",
            [RegistryPermission::Read, RegistryPermission::ProcessSpawn],
            false,
        ),
        tool_entry(
            ToolIdentity {
                states,
                name: "typescript_semantic",
            },
            Capability::CodeIntelligence,
            "Run read-only TypeScript/JavaScript definitions, references, diagnostics, or workspace-symbol queries through the repository-scoped language server.",
            json!({"type":"object","properties":{"operation":{"type":"string","enum":["definition","references","diagnostics","workspace_symbols"]},"path":{"type":"string"},"line":{"type":"integer","minimum":0},"character":{"type":"integer","minimum":0},"query":{"type":"string"}},"required":["operation"],"additionalProperties":false}),
            "medusa-agent::tools::intelligence::typescript_semantic",
            [RegistryPermission::Read, RegistryPermission::ProcessSpawn],
            false,
        ),
        tool_entry(
            ToolIdentity {
                states,
                name: "patch_apply",
            },
            Capability::Filesystem,
            "Apply a guarded atomic multi-file byte-range patch transaction.",
            json!({"type":"object","properties":{"edits":{"type":"array","items":{"type":"object","properties":{"path":{"type":"string"},"start_byte":{"type":"integer","minimum":0},"end_byte":{"type":"integer","minimum":0},"expected":{"type":"string"},"replacement":{"type":"string"}},"required":["path","start_byte","end_byte","expected","replacement"],"additionalProperties":false}}},"required":["edits"],"additionalProperties":false}),
            "medusa-agent::tools::intelligence::patch_apply",
            [
                RegistryPermission::Write,
                RegistryPermission::RepositoryMutation,
            ],
            true,
        ),
        tool_entry(
            ToolIdentity {
                states,
                name: "symbol_rename",
            },
            Capability::CodeIntelligence,
            "Guardedly rename one unambiguous Rust or TypeScript/JavaScript symbol; fail closed on parse/protocol errors, ambiguity, repository scope drift, incomplete references, or stale bytes.",
            json!({"type":"object","properties":{"old_name":{"type":"string"},"new_name":{"type":"string"}},"required":["old_name","new_name"],"additionalProperties":false}),
            "medusa-agent::tools::intelligence::symbol_rename",
            [
                RegistryPermission::Write,
                RegistryPermission::RepositoryMutation,
                RegistryPermission::ProcessSpawn,
            ],
            true,
        ),
        tool_entry(
            ToolIdentity {
                states,
                name: "shell_run",
            },
            Capability::Shell,
            "Run an approved executable directly in the repository and capture output.",
            json!({"type":"object","properties":{"program":{"type":"string"},"args":{"type":"array","items":{"type":"string"}},"output_mode":{"type":"string","enum":["compact","normal","verbatim"],"default":"compact"}},"required":["program","args"],"additionalProperties":false}),
            "medusa-agent::tools::shell::run",
            [RegistryPermission::Read, RegistryPermission::ProcessSpawn],
            false,
        ),
        tool_entry(
            ToolIdentity {
                states,
                name: "web_search",
            },
            Capability::Network,
            "Search the public web for current information.",
            json!({"type":"object","properties":{"query":{"type":"string"},"allowed_domains":{"type":"array","items":{"type":"string"}},"blocked_domains":{"type":"array","items":{"type":"string"}}},"required":["query"],"additionalProperties":false}),
            "medusa-agent::tools::web::search",
            [RegistryPermission::Read, RegistryPermission::Network],
            false,
        ),
        tool_entry(
            ToolIdentity {
                states,
                name: "web_fetch",
            },
            Capability::Network,
            "Fetch a public HTTP(S) page and return readable text.",
            json!({"type":"object","properties":{"url":{"type":"string"},"prompt":{"type":"string"}},"required":["url"],"additionalProperties":false}),
            "medusa-agent::tools::web::fetch",
            [RegistryPermission::Read, RegistryPermission::Network],
            false,
        ),
        tool_entry(
            ToolIdentity {
                states,
                name: "skill_read",
            },
            Capability::Filesystem,
            "Read an available instruction-only plugin before applying it.",
            json!({"type":"object","properties":{"name":{"type":"string"},"scope":{"type":"string","enum":["project","user"]}},"required":["name"],"additionalProperties":false}),
            "medusa-agent::tools::skills::read",
            [RegistryPermission::Read],
            false,
        ),
        tool_entry(
            ToolIdentity {
                states,
                name: "skill_execute",
            },
            Capability::Shell,
            "Execute one explicitly validated executable skill package in a temporary contained copy with bounded JSON input and output.",
            json!({"type":"object","properties":{"name":{"type":"string"},"entrypoint":{"type":"string"},"input":{"type":"object"}},"required":["name","entrypoint"],"additionalProperties":false}),
            "medusa-agent::tools::executable_skills::run",
            [RegistryPermission::Read, RegistryPermission::ProcessSpawn],
            false,
        ),
        tool_entry(
            ToolIdentity {
                states,
                name: "update_plan",
            },
            Capability::Memory,
            "Create or update the visible task plan without modifying repository files.",
            json!({"type":"object","properties":{"steps":{"type":"array","minItems":1,"maxItems":8,"items":{"type":"object","properties":{"title":{"type":"string"},"status":{"type":"string","enum":["pending","in_progress","completed","failed"]}},"required":["title","status"],"additionalProperties":false}}},"required":["steps"],"additionalProperties":false}),
            "medusa-agent::engine::update_plan",
            [RegistryPermission::RuntimeMutation],
            false,
        ),
        tool_entry(
            ToolIdentity {
                states,
                name: "ask_user_question",
            },
            Capability::Memory,
            "Ask one to four blocking multiple-choice clarification questions.",
            json!({"type":"object","properties":{"questions":{"type":"array","minItems":1,"maxItems":4,"items":{"type":"object","properties":{"header":{"type":"string","maxLength":12},"question":{"type":"string"},"options":{"type":"array","minItems":2,"maxItems":4,"items":{"type":"object","properties":{"label":{"type":"string"},"description":{"type":"string"}},"required":["label","description"],"additionalProperties":false}},"multiSelect":{"type":"boolean"}},"required":["header","question","options"],"additionalProperties":false}}},"required":["questions"],"additionalProperties":false}),
            "medusa-agent::engine::ask_user_question",
            [RegistryPermission::RuntimeMutation],
            false,
        ),
        tool_entry(
            ToolIdentity {
                states,
                name: "git_checkpoint",
            },
            Capability::Git,
            "Stage all changes and create a Git checkpoint commit.",
            json!({"type":"object","properties":{"message":{"type":"string"}},"required":["message"],"additionalProperties":false}),
            "medusa-agent::tools::git::checkpoint",
            [
                RegistryPermission::Write,
                RegistryPermission::RepositoryMutation,
            ],
            true,
        ),
    ];
    if desktop.enabled() {
        let allowed = desktop
            .effective_tools_for_mode(false)
            .into_iter()
            .map(Value::String)
            .collect::<Vec<_>>();
        entries.push(tool_entry(
            ToolIdentity { states, name: "desktop_commander" },
            Capability::DesktopMcp,
            "Call one policy-approved Desktop Commander MCP file or search tool.",
            json!({"type":"object","properties":{"tool":{"type":"string","enum":allowed},"arguments":{"type":"object"}},"required":["tool","arguments"],"additionalProperties":false}),
            "medusa-agent::engine::desktop_commander",
            [RegistryPermission::Read, RegistryPermission::ProcessSpawn],
            false,
        ));
    }
    entries
}

struct ToolIdentity<'a> {
    states: &'a BTreeMap<Capability, CapabilityState>,
    name: &'a str,
}

fn tool_entry<const N: usize>(
    identity: ToolIdentity<'_>,
    capability: Capability,
    description: &str,
    input_schema: Value,
    handler: &str,
    permissions: [RegistryPermission; N],
    requires_approval: bool,
) -> RegistryEntry {
    let ToolIdentity { states, name } = identity;
    let state = states.get(&capability).cloned().unwrap_or(CapabilityState {
        available: false,
        detail: "capability was not discovered".into(),
    });
    let readiness_detail = state.detail.clone();
    let permissions = BTreeSet::from(permissions);
    RegistryEntry {
        id: format!("tool.{name}"),
        capability,
        kind: RegistryKind::Tool,
        status: CapabilityStatus::Production,
        description: description.into(),
        owner: "agent tool maintainers".into(),
        lifecycle_owner: "medusa-agent::ToolManager".into(),
        surfaces: BTreeSet::from([
            CapabilitySurface::Model,
            CapabilitySurface::Cli,
            CapabilitySurface::Ui,
            CapabilitySurface::Protocol,
            CapabilitySurface::Documentation,
        ]),
        permissions: permissions.clone(),
        explicit_approval: if requires_approval {
            permissions
                .iter()
                .copied()
                .filter(|permission| {
                    matches!(
                        permission,
                        RegistryPermission::Write
                            | RegistryPermission::RepositoryMutation
                            | RegistryPermission::RuntimeMutation
                    )
                })
                .collect()
        } else {
            BTreeSet::new()
        },
        dependencies: vec![capability.label().into()],
        supported_platforms: supported_platforms(),
        readiness: ReadinessContract {
            ready: state.available,
            detail: state.detail,
            evidence: vec![
                format!("capability admission: {readiness_detail}"),
                format!("registered handler {handler}"),
            ],
        },
        handler: Some(HandlerRegistration {
            id: handler.into(),
            lifecycle_owner: "medusa-agent::ToolManager".into(),
        }),
        tool: Some(ToolDefinition {
            name: name.into(),
            description: description.into(),
            input_schema,
        }),
        provenance: Some("built-in registry".into()),
    }
}

fn mutating(entry: &RegistryEntry) -> bool {
    entry.permissions.iter().any(|permission| {
        matches!(
            permission,
            RegistryPermission::Write | RegistryPermission::RepositoryMutation
        )
    })
}

fn supported_platforms() -> BTreeSet<String> {
    BTreeSet::from(["linux".into(), "macos".into(), "windows".into()])
}

fn insert_state(
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

fn valid_id(id: &str) -> bool {
    id.bytes().all(|byte| {
        byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'-' | b'_')
    })
}

fn io_error(error: std::io::Error) -> MedusaError {
    MedusaError::new(
        ErrorCode::PersistenceFailed,
        ErrorCategory::Environment,
        error.to_string(),
    )
}

fn invalid(message: impl Into<String>) -> MedusaError {
    MedusaError::new(
        ErrorCode::InvalidConfiguration,
        ErrorCategory::Validation,
        message.into(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeProbe(BTreeSet<String>);

    impl CommandProbe for FakeProbe {
        fn available(&self, program: &str, _: &[&str]) -> bool {
            self.0.contains(program)
        }
    }

    fn ready_registry() -> CapabilityRegistry {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.keep();
        CapabilityRegistry::discover_with(
            path,
            &FakeProbe(BTreeSet::from([
                "git".into(),
                "gh".into(),
                "node".into(),
                if cfg!(windows) {
                    "cmd".into()
                } else {
                    "sh".into()
                },
            ])),
        )
        .expect("registry")
    }

    fn registry_with_browser_state(state: CapabilityState) -> CapabilityRegistry {
        let mut registry = ready_registry();
        registry.capabilities.insert(Capability::Browser, state);
        registry.entries = build_entries(
            &registry.capabilities,
            &DesktopCommanderSettings::from_env(),
        );
        registry.validate().expect("browser registry");
        registry
    }

    fn configured_browser() -> medusa_config::BrowserConfig {
        medusa_config::BrowserConfig {
            enabled: true,
            path: Some(PathBuf::from("test-browserd")),
            timeout_ms: 30_000,
        }
    }

    fn browser_probe() -> FakeProbe {
        FakeProbe(BTreeSet::from(["test-browserd".into(), "node".into()]))
    }

    #[test]
    fn every_model_tool_has_one_ready_handler() {
        let registry = ready_registry();
        let tools = registry.model_tools(false);
        assert!(!tools.is_empty());
        for tool in tools {
            let entry = registry
                .entries
                .values()
                .find(|entry| {
                    entry
                        .tool
                        .as_ref()
                        .is_some_and(|item| item.name == tool.name)
                })
                .expect("registry entry");
            assert!(entry.readiness.ready);
            assert!(entry.handler.is_some());
        }
    }

    #[test]
    fn code_intelligence_tools_have_a_dedicated_truthful_capability() {
        let registry = ready_registry();
        assert!(registry.available(Capability::CodeIntelligence));
        assert_eq!(
            registry
                .entry("tool.semantic_capabilities")
                .expect("report")
                .capability,
            Capability::CodeIntelligence
        );
        assert_eq!(
            registry.entry("tool.code_index").expect("index").capability,
            Capability::CodeIntelligence
        );
        let rename = registry.entry("tool.symbol_rename").expect("rename");
        assert_eq!(rename.capability, Capability::CodeIntelligence);
        assert!(
            rename
                .permissions
                .contains(&RegistryPermission::RepositoryMutation)
        );
        assert!(
            rename
                .permissions
                .contains(&RegistryPermission::ProcessSpawn)
        );
        assert!(
            registry
                .model_tools(true)
                .iter()
                .any(|tool| tool.name == "semantic_capabilities")
        );
        assert!(
            !registry
                .model_tools(true)
                .iter()
                .any(|tool| tool.name == "symbol_rename")
        );
    }

    #[test]
    fn browser_tools_remain_quarantined_without_medusa_verification_route() {
        let state = browser_capability_state(&configured_browser(), None, &browser_probe());
        assert!(!state.available);
        let registry = registry_with_browser_state(state);
        assert!(
            registry
                .model_tools(false)
                .iter()
                .all(|tool| !tool.name.starts_with("browser_"))
        );
    }

    #[test]
    fn invalid_browser_routes_never_project_model_tools() {
        for route in [
            " ",
            "not a url",
            "file:///tmp/index.html",
            "http://user:secret@localhost:4173/app",
            "http://localhost:4173/app#fragment",
            "http://10.0.0.1/verify",
            "C:\\work\\app\\index.html",
        ] {
            let state = browser_capability_state(
                &configured_browser(),
                Some(route),
                &browser_probe(),
            );
            assert!(!state.available, "accepted {route}: {}", state.detail);
            assert!(state.detail.contains("verification route"));
            let registry = registry_with_browser_state(state);
            assert!(
                registry
                    .model_tools(false)
                    .iter()
                    .all(|tool| !tool.name.starts_with("browser_")),
                "projected browser tool for {route}"
            );
        }
    }

    #[test]
    fn verified_browser_state_exposes_only_bounded_model_actions() {
        let route = " HTTP://LOCALHOST:4173/app?mode=verify ";
        let admitted = VerificationRoute::parse(route).expect("admitted route");
        let state = browser_capability_state(
            &configured_browser(),
            Some(route),
            &browser_probe(),
        );
        assert!(state.available);
        assert!(state.detail.contains(&admitted.safe_fingerprint()));
        assert!(!state.detail.contains("mode=verify"));
        let registry = registry_with_browser_state(state);
        let names = registry
            .model_tools(false)
            .into_iter()
            .filter(|tool| tool.name.starts_with("browser_"))
            .map(|tool| tool.name)
            .collect::<BTreeSet<_>>();
        assert_eq!(
            names,
            BTreeSet::from([
                "browser_close".to_owned(),
                "browser_click".to_owned(),
                "browser_fill".to_owned(),
                "browser_navigate".to_owned(),
                "browser_ping".to_owned(),
                "browser_press".to_owned(),
                "browser_screenshot".to_owned(),
                "browser_snapshot".to_owned(),
                "browser_tabs".to_owned(),
            ])
        );
        assert!(!names.contains("browser_evaluate"));
        assert!(
            registry
                .model_tools(true)
                .iter()
                .all(|tool| !tool.name.starts_with("browser_"))
        );
        for name in &names {
            let entry = registry
                .entry(&format!("tool.{name}"))
                .expect("browser entry");
            assert!(entry.readiness.ready);
            assert_eq!(entry.status, CapabilityStatus::Production);
            assert!(entry.handler.is_some());
            assert!(
                entry
                    .readiness
                    .evidence
                    .iter()
                    .any(|item| item.contains(&admitted.safe_fingerprint()))
            );
            assert!(
                entry
                    .readiness
                    .evidence
                    .iter()
                    .all(|item| !item.contains("mode=verify"))
            );
            let schema = &entry.tool.as_ref().expect("tool schema").input_schema;
            assert_eq!(
                schema.get("additionalProperties"),
                Some(&Value::Bool(false))
            );
            assert!(
                !schema
                    .get("properties")
                    .and_then(Value::as_object)
                    .is_some_and(|properties| properties.contains_key("verified"))
            );
        }
    }

    #[test]
    fn browser_readiness_requires_sidecar_and_node_probes() {
        assert!(
            !browser_capability_state(
                &configured_browser(),
                Some("http://127.0.0.1:4173"),
                &FakeProbe(BTreeSet::from(["node".into()])),
            )
            .available
        );
        assert!(
            !browser_capability_state(
                &configured_browser(),
                Some("http://127.0.0.1:4173"),
                &FakeProbe(BTreeSet::from(["test-browserd".into()])),
            )
            .available
        );
    }

    #[test]
    fn duplicate_tool_names_and_missing_handlers_fail_closed() {
        let registry = ready_registry();
        let mut entries = registry.entries.values().cloned().collect::<Vec<_>>();
        let mut duplicate = entries[0].clone();
        duplicate.id = "tool.duplicate".into();
        entries.push(duplicate);
        assert!(
            CapabilityRegistry::from_entries(
                registry.repository.clone(),
                registry.capabilities.clone(),
                entries
            )
            .is_err()
        );

        let mut broken = registry.entries.values().cloned().collect::<Vec<_>>();
        broken[0].handler = None;
        assert!(
            CapabilityRegistry::from_entries(registry.repository, registry.capabilities, broken)
                .is_err()
        );
    }

    #[test]
    fn unavailable_dependencies_remove_tools_from_all_executable_projections() {
        let directory = tempfile::tempdir().expect("tempdir");
        let registry =
            CapabilityRegistry::discover_with(directory.path().into(), &FakeProbe(BTreeSet::new()))
                .expect("registry");
        let names = registry
            .model_tools(false)
            .into_iter()
            .map(|tool| tool.name)
            .collect::<BTreeSet<_>>();
        assert!(!names.contains("shell_run"));
        assert!(!names.contains("git_checkpoint"));
        assert!(!names.contains("web_search") || registry.available(Capability::Network));
    }
}
