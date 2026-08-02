#!/usr/bin/env python3
from __future__ import annotations

import json
from pathlib import Path


def replace_once(text: str, old: str, new: str, label: str) -> str:
    count = text.count(old)
    if count != 1:
        raise RuntimeError(f"{label}: expected one match, found {count}")
    return text.replace(old, new, 1)


def update_tools() -> None:
    path = Path("crates/medusa-agent/src/tools/mod.rs")
    text = path.read_text(encoding="utf-8")
    text = replace_once(
        text,
        "use std::path::Path;\n\nuse medusa_core",
        "use std::path::{Path, PathBuf};\n\nuse medusa_capabilities::{CapabilityRegistry, SystemProbe};\nuse medusa_core",
        "tool registry imports",
    )
    text = replace_once(
        text,
        "use serde_json::{Value, json};",
        "use serde_json::Value;\n#[cfg(test)]\nuse serde_json::json;",
        "scope hard-coded schema macro to tests",
    )
    start = text.index("/// Single policy-aware registry")
    end = text.index("pub(crate) fn execute_tool", start)
    prefix = '''/// Single policy-aware registry for built-in tools shared by every agent frontend.
#[derive(Clone, Debug)]
pub struct ToolManager {
    desktop_commander: DesktopCommanderSettings,
}

impl ToolManager {
    #[must_use]
    pub fn new(desktop_commander: DesktopCommanderSettings) -> Self {
        Self { desktop_commander }
    }

    /// Compatibility projection for callers without a repository context. Discovery failure
    /// fails closed by returning no model tools.
    #[must_use]
    pub fn definitions(&self, read_only: bool) -> Vec<ToolDefinition> {
        CapabilityRegistry::discover_with_desktop(
            PathBuf::from("."),
            &SystemProbe,
            self.desktop_commander.clone(),
        )
        .map_or_else(|_| Vec::new(), |registry| registry.model_tools(read_only))
    }

    pub fn definitions_for(
        &self,
        repo: &Path,
        read_only: bool,
    ) -> MedusaResult<Vec<ToolDefinition>> {
        built_in_tools(repo, &self.desktop_commander, read_only)
    }

    pub fn execute(&self, repo: &Path, name: &str, input: &Value) -> MedusaResult<String> {
        let registry = CapabilityRegistry::discover_with_desktop(
            repo.to_path_buf(),
            &SystemProbe,
            self.desktop_commander.clone(),
        )?;
        let id = format!("tool.{name}");
        let entry = registry.entry(&id).ok_or_else(|| invalid_tool(format!(
            "tool is not registered: {name}"
        )))?;
        if !entry.projected_to(medusa_capabilities::CapabilitySurface::Model) {
            return Err(MedusaError::new(
                ErrorCode::PolicyDenied,
                ErrorCategory::Policy,
                format!("tool is unavailable: {name}: {}", entry.readiness.detail),
            ));
        }
        execute_tool(repo, name, input)
    }

    pub fn execute_approved(&self, repo: &Path, name: &str, input: &Value) -> MedusaResult<String> {
        execute_approved_tool(repo, name, input)
    }
}

pub(crate) fn available_skills(repo: &Path) -> Vec<skills::SkillSummary> {
    skills::summaries(repo)
}

pub(crate) fn built_in_tools(
    repo: &Path,
    desktop_commander: &DesktopCommanderSettings,
    read_only: bool,
) -> MedusaResult<Vec<ToolDefinition>> {
    CapabilityRegistry::discover_with_desktop(
        repo.to_path_buf(),
        &SystemProbe,
        desktop_commander.clone(),
    )
    .map(|registry| registry.model_tools(read_only))
}

'''
    text = text[:start] + prefix + text[end:]
    tool_helper = '''fn tool(name: &str, description: &str, input_schema: Value) -> ToolDefinition {
    ToolDefinition {
        name: name.into(),
        description: description.into(),
        input_schema,
    }
}

'''
    if tool_helper in text:
        text = text.replace(tool_helper, "", 1)
    path.write_text(text, encoding="utf-8")


def update_engine_support() -> None:
    path = Path("crates/medusa-agent/src/engine_support.rs")
    text = path.read_text(encoding="utf-8")
    old = '''pub(crate) fn available_tools(
    mode: Mode,
    desktop_commander: &DesktopCommanderSettings,
) -> Vec<medusa_provider::ToolDefinition> {
    built_in_tools(desktop_commander, mode == Mode::ReadOnly)
        .into_iter()
        .filter(|tool| tool_allowed(mode, &tool.name))
        .collect()
}'''
    new = '''pub(crate) fn available_tools(
    mode: Mode,
    repo: &Path,
    desktop_commander: &DesktopCommanderSettings,
) -> MedusaResult<Vec<medusa_provider::ToolDefinition>> {
    built_in_tools(repo, desktop_commander, mode == Mode::ReadOnly).map(|tools| {
        tools
            .into_iter()
            .filter(|tool| tool_allowed(mode, &tool.name))
            .collect()
    })
}'''
    text = replace_once(text, old, new, "agent tool projection")
    text = replace_once(
        text,
        '''            let tools = available_tools(mode, &DesktopCommanderSettings::default())
                .into_iter()
                .map(|tool| tool.name)
                .collect::<Vec<_>>();''',
        '''            let directory = tempfile::tempdir().expect("tempdir");
            let tools = available_tools(
                mode,
                directory.path(),
                &DesktopCommanderSettings::default(),
            )
            .expect("available tools")
            .into_iter()
            .map(|tool| tool.name)
            .collect::<Vec<_>>();''',
        "repo-aware agent tool projection test",
    )
    path.write_text(text, encoding="utf-8")


def update_engine() -> None:
    path = Path("crates/medusa-agent/src/engine.rs")
    text = path.read_text(encoding="utf-8")
    text = replace_once(
        text,
        "let mut tools = available_tools(self.config.agent.mode, &self.desktop_commander_settings);",
        "let mut tools = available_tools(\n            self.config.agent.mode,\n            &session.repo,\n            &self.desktop_commander_settings,\n        )?;",
        "engine registry projection",
    )
    path.write_text(text, encoding="utf-8")


def update_cli() -> None:
    path = Path("crates/medusa-cli/src/capabilities.rs")
    text = path.read_text(encoding="utf-8")
    text = replace_once(
        text,
        "    ExplicitCapabilityRuntime, github_descriptor,",
        "    github_descriptor,",
        "remove legacy diagnostics runtime import",
    )
    old = '''fn diagnostics(repository_path: PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    let repository_name =
        env::var("MEDUSA_GITHUB_REPOSITORY").unwrap_or_else(|_| "local/repository".into());
    let registry = CapabilityRegistry::discover(&repository_path)?;
    let grants = vec![
        CapabilityGrant {
            capability: Capability::GitHub,
            permissions: BTreeSet::from([CapabilityPermission::Read]),
        },
        CapabilityGrant {
            capability: Capability::SelfImprovement,
            permissions: BTreeSet::from([CapabilityPermission::Read]),
        },
    ];
    let runtime =
        ExplicitCapabilityRuntime::new(registry, grants, GitHubService::new(repository_name));
    println!("{}", serde_json::to_string_pretty(&runtime.diagnostics())?);
    Ok(())
}'''
    new = '''fn diagnostics(repository_path: PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    let registry = CapabilityRegistry::discover(&repository_path)?;
    println!(
        "{}",
        serde_json::to_string_pretty(&registry.protocol_report())?
    );
    Ok(())
}'''
    text = replace_once(text, old, new, "CLI protocol report")
    path.write_text(text, encoding="utf-8")


def update_conformance() -> None:
    path = Path("scripts/architecture-conformance.py")
    text = path.read_text(encoding="utf-8")
    start = text.index("\ndef browser_dispatch_unreachable")
    end = text.index("\ndef integration_precedes_parent_review", start)
    text = text[:start] + text[end:]
    text = replace_once(
        text,
        'PROBES: dict[str, Callable[[Path], tuple[bool, str]]] = {\n    "browser-dispatch-unreachable": browser_dispatch_unreachable,',
        'PROBES: dict[str, Callable[[Path], tuple[bool, str]]] = {',
        "remove resolved browser fixture",
    )
    path.write_text(text, encoding="utf-8")


def update_baseline() -> None:
    path = Path("docs/architecture/baseline.json")
    payload = json.loads(path.read_text(encoding="utf-8"))
    payload["known_failure_fixtures"] = [
        row for row in payload["known_failure_fixtures"] if row[0] != "browser-dispatch-unreachable"
    ]
    for row in payload["capabilities"]:
        if row[0] == "browser-tools":
            row[1] = "withheld"
            row[2] = "partial"
            row[3] = "adapt"
            row[4] = "v2 registry protocol/documentation record; no executable projection"
            row[5] = ["production dispatcher and behavioral certification remain required"]
        elif row[0] == "plugins-and-extensions":
            row[1] = "managed"
            row[2] = "partial"
            row[3] = "adapt"
            row[4] = "managed plugin manifest plus instruction-only SKILL.md compatibility"
            row[5] = ["executable plugin dispatchers require later certification"]
    for row in payload["sources_of_truth"]:
        if row[0] == "capability availability":
            row[1] = "medusa-capabilities versioned registry snapshot"
            row[2] = ["generated CLI, model, UI, protocol and documentation projections"]
            row[3] = "versioned capability registry"
    payload["capability_registry"] = {
        "schema_version": 2,
        "authority": "crates/medusa-capabilities",
        "plugin_authority": "crates/medusa-extensions/src/plugins.rs",
        "model_projection": "medusa-capabilities::CapabilityRegistry::model_tools",
        "protocol_projection": "medusa-capabilities::CapabilityRegistry::protocol_report",
        "browser_policy": "withheld from executable surfaces until a certified handler is registered",
    }
    path.write_text(json.dumps(payload, indent=2) + "\n", encoding="utf-8")


def update_index() -> None:
    path = Path("docs/architecture/INDEX.md")
    text = path.read_text(encoding="utf-8")
    text = replace_once(
        text,
        "The legacy capability ledger remains in [`../CAPABILITY-CLAIMS.json`](../CAPABILITY-CLAIMS.json) and [`../CAPABILITY-EVIDENCE.md`](../CAPABILITY-EVIDENCE.md). Its `production` value means a current entrypoint exists; it does **not** mean architecture-v2 certification.",
        "The versioned runtime authority is `medusa-capabilities::CapabilityRegistry`. Model tools, prompt availability, CLI diagnostics, protocol reports, and generated documentation are projections of one validated snapshot. Historical claim documents are migration evidence only and no longer grant runtime availability.",
        "architecture capability authority",
    )
    text = replace_once(
        text,
        "| Browser tools | advertised | quarantined | replace | no production `execute_tool` dispatch |",
        "| Browser tools | withheld | partial | adapt | registry records remain non-executable until a production handler is certified |",
        "browser capability row",
    )
    text = replace_once(
        text,
        "| Plugins/extensions | structural | design-only | adapt | no certified manifest/permissions/lifecycle |",
        "| Plugins/extensions | managed | partial | adapt | versioned manifests and instruction-only `SKILL.md`; executable handlers still require certification |",
        "plugin capability row",
    )
    text = text.replace("- `browser-dispatch-unreachable` (#631)\n", "")
    decision = "- Decision: [`decisions/0003-truthful-capability-plugin-registry.md`](decisions/0003-truthful-capability-plugin-registry.md)\n"
    marker = "- Decision: [`decisions/0002-verified-prebuilt-updates.md`](decisions/0002-verified-prebuilt-updates.md)\n"
    text = replace_once(text, marker, marker + decision, "registry ADR link")
    path.write_text(text, encoding="utf-8")


if __name__ == "__main__":
    update_tools()
    update_engine_support()
    update_engine()
    update_cli()
    update_conformance()
    update_baseline()
    update_index()
