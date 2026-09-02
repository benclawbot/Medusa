#!/usr/bin/env python3
from pathlib import Path

claims = Path("docs/CAPABILITY-CLAIMS.json")
text = claims.read_text(encoding="utf-8")
replacements = {
    "crates/medusa-runtime/src/multi_agent_coordinator.rs": "crates/medusa-runtime/src/coordination/multi_agent_coordinator.rs",
    "crates/medusa-runtime/src/mutating_worker_coordinator.rs": "crates/medusa-runtime/src/coordination/mutating_worker_coordinator.rs",
    "crates/medusa-runtime/src/production_orchestrator.rs": "crates/medusa-runtime/src/coordination/production_orchestrator.rs",
}
for old, new in replacements.items():
    if old not in text:
        raise SystemExit(f"capability claim path not found: {old}")
    text = text.replace(old, new)
claims.write_text(text, encoding="utf-8")

glossary = Path("docs/glossary.md")
text = glossary.read_text(encoding="utf-8")
old_link = "[`crates/medusa-runtime` `integration_barrier.rs`](../crates/medusa-runtime/src/integration_barrier.rs)."
new_link = "[`medusa-multi-agent-scheduler::mutation_dag`](../crates/medusa-multi-agent-scheduler/src/mutation_dag.rs)."
if old_link not in text:
    raise SystemExit("stale IntegrationBarrier glossary link not found")
text = text.replace(old_link, new_link, 1)
stale_manifest = """- **EffectiveModelRequestManifest** — provider-neutral record of what was actually
  requested (model, route, role, inputs, evidence references). Enables deterministic
  request reconstruction across providers. See
  [`crates/medusa-runtime/src/effective_model_request_manifest.rs`](../crates/medusa-runtime/src/effective_model_request_manifest.rs).
"""
if stale_manifest not in text:
    raise SystemExit("stale EffectiveModelRequestManifest glossary entry not found")
text = text.replace(stale_manifest, "", 1)
glossary.write_text(text, encoding="utf-8")

architecture = Path("scripts/check-product-architecture.py")
text = architecture.read_text(encoding="utf-8")
for old, new in {
    'read(root, "crates/medusa-runtime/src/production_orchestrator.rs")': 'read(root, "crates/medusa-runtime/src/coordination/production_orchestrator.rs")',
    'read(root, "crates/medusa-runtime/src/multi_agent_coordinator.rs")': 'read(root, "crates/medusa-runtime/src/coordination/multi_agent_coordinator.rs")',
    'read(root, "crates/medusa-runtime/src/mutating_worker_coordinator.rs")': 'read(root, "crates/medusa-runtime/src/coordination/mutating_worker_coordinator.rs")',
}.items():
    if old not in text:
        raise SystemExit(f"product architecture source path not found: {old}")
    text = text.replace(old, new)
old = '    runtime = read(root, "crates/medusa-runtime/src/lib.rs")\n    readme = read(root, "README.md")'
new = '    runtime = read(root, "crates/medusa-runtime/src/lib.rs")\n    coordination = read(root, "crates/medusa-runtime/src/coordination/mod.rs")\n    readme = read(root, "README.md")'
if old not in text:
    raise SystemExit("product architecture runtime read site not found")
text = text.replace(old, new, 1)
old = '''    require(runtime, "mod production_orchestrator;", "runtime root")
    require(runtime, "pub mod orchestration_planning", "runtime root")
    forbid(runtime, "pub mod production_orchestrator;", "runtime root")
'''
new = '''    require(runtime, "pub(crate) mod coordination;", "runtime root")
    require(coordination, "pub(crate) mod multi_agent_coordinator;", "coordination root")
    require(coordination, "pub(crate) mod mutating_worker_coordinator;", "coordination root")
    require(coordination, "pub mod production_orchestrator;", "coordination root")
    require(runtime, "pub mod orchestration_planning", "runtime root")
    forbid(runtime, "pub mod production_orchestrator;", "runtime root")
'''
if old not in text:
    raise SystemExit("product architecture module authority block not found")
text = text.replace(old, new, 1)
architecture.write_text(text, encoding="utf-8")

update = Path("crates/medusa-cli/src/update_command.rs")
text = update.read_text(encoding="utf-8")
marker = '''#[cfg(test)]
fn render_progress_line(
'''
struct = '''struct ProgressLine<'a> {
    stage: UpdateStage,
    stage_label: &'a str,
    percent: u8,
    detail: &'a str,
    current_version: &'a str,
    new_version: &'a str,
    colors: bool,
}

#[cfg(test)]
fn render_progress_line(
'''
if text.count(marker) != 1:
    raise SystemExit("progress line insertion marker not found exactly once")
text = text.replace(marker, struct, 1)
old = '''    render_progress_line_with_width(
        stage,
        stage.label(),
        percent,
        detail,
        current_version,
        new_version,
        colors,
        usize::MAX,
    )
'''
new = '''    render_progress_line_with_width(
        ProgressLine {
            stage,
            stage_label: stage.label(),
            percent,
            detail,
            current_version,
            new_version,
            colors,
        },
        usize::MAX,
    )
'''
if text.count(old) != 1:
    raise SystemExit("test render helper call not found exactly once")
text = text.replace(old, new, 1)
old = '''fn render_progress_line_with_width(
    stage: UpdateStage,
    stage_label: &str,
    percent: u8,
    detail: &str,
    current_version: &str,
    new_version: &str,
    colors: bool,
    terminal_width: usize,
) -> String {
'''
new = '''fn render_progress_line_with_width(line: ProgressLine<'_>, terminal_width: usize) -> String {
    let ProgressLine {
        stage,
        stage_label,
        percent,
        detail,
        current_version,
        new_version,
        colors,
    } = line;
'''
if text.count(old) != 1:
    raise SystemExit("progress renderer signature not found exactly once")
text = text.replace(old, new, 1)
old = '''            render_progress_line_with_width(
                self.stage,
                &self.stage_label,
                percent,
                &self.detail,
                &self.current_version,
                &self.new_version,
                self.colors,
                self.terminal_width,
            )
'''
new = '''            render_progress_line_with_width(
                ProgressLine {
                    stage: self.stage,
                    stage_label: &self.stage_label,
                    percent,
                    detail: &self.detail,
                    current_version: &self.current_version,
                    new_version: &self.new_version,
                    colors: self.colors,
                },
                self.terminal_width,
            )
'''
if text.count(old) != 1:
    raise SystemExit("runtime progress renderer call not found exactly once")
text = text.replace(old, new, 1)
test_calls = [
    ('''        let line = render_progress_line_with_width(
            UpdateStage::Building,
            "Building 235/305 crates",
            77,
            "02:10 elapsed · medusa-runtime",
            "1.0.6 (old)",
            "1.0.7 (new)",
            false,
            120,
        );
''', '''        let line = render_progress_line_with_width(
            ProgressLine {
                stage: UpdateStage::Building,
                stage_label: "Building 235/305 crates",
                percent: 77,
                detail: "02:10 elapsed · medusa-runtime",
                current_version: "1.0.6 (old)",
                new_version: "1.0.7 (new)",
                colors: false,
            },
            120,
        );
'''),
    ('''        let line = render_progress_line_with_width(
            UpdateStage::Downloading,
            "Downloading",
            42,
            "123.4 MiB / 567.8 MiB · 12.4 MiB/s · ETA 00:37",
            "1.0.5 (5b97a73ef0d4)",
            "1.0.5 (5c17d7f00f4f)",
            false,
            80,
        );
''', '''        let line = render_progress_line_with_width(
            ProgressLine {
                stage: UpdateStage::Downloading,
                stage_label: "Downloading",
                percent: 42,
                detail: "123.4 MiB / 567.8 MiB · 12.4 MiB/s · ETA 00:37",
                current_version: "1.0.5 (5b97a73ef0d4)",
                new_version: "1.0.5 (5c17d7f00f4f)",
                colors: false,
            },
            80,
        );
'''),
]
for old, new in test_calls:
    if text.count(old) != 1:
        raise SystemExit("progress renderer test call not found exactly once")
    text = text.replace(old, new, 1)
update.write_text(text, encoding="utf-8")
