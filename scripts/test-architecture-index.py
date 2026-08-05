#!/usr/bin/env python3
"""Adversarial fixtures for the final Architecture v2 certification checker."""
from __future__ import annotations
import importlib.util, json, tempfile, unittest
from pathlib import Path

SCRIPT=Path(__file__).with_name("check-architecture-index.py")
SPEC=importlib.util.spec_from_file_location("check_architecture_index",SCRIPT)
assert SPEC and SPEC.loader
CHECKER=importlib.util.module_from_spec(SPEC); SPEC.loader.exec_module(CHECKER)
INDEX_SECTIONS="\n".join(sorted(CHECKER.REQUIRED_INDEX_SECTIONS))
PR_TEXT="\n".join(sorted(CHECKER.REQUIRED_PR_TEXT))
CODEOWNERS="\n".join(f"{path} @owner" for path in sorted(CHECKER.REQUIRED_CODEOWNERS))
EXECUTION=["plan","lease","implement-isolated","verify-changed-paths","review-prepared-change","verify-independently","authorize","integrate-accepted-change","reconcile","persist-terminal-completion"]
ENTRYPOINT="dedicated durable parent reviewer -> independent verification -> authorization -> integration -> reconciliation -> canonical terminal persistence"

class Fixture:
    def __init__(self)->None:
        self.temp=tempfile.TemporaryDirectory(); self.root=Path(self.temp.name)
        self.write_cargo(["crates/medusa-core"])
        self.write("crates/medusa-core/Cargo.toml",'[package]\nname="medusa-core"\nversion="0.0.0"\n')
        self.write("crates/medusa-runtime/src/mutation_transaction_state.rs","// authoritative fixture\n")
        self.write("docs/architecture/owners.json",json.dumps({"schema_version":1,"owners":{"medusa-core":"foundation"}}))
        for path in ("docs/ARCHITECTURE.md","docs/CONTRIBUTOR-ARCHITECTURE.md","docs/architecture/LEGACY-DELETION.md","docs/architecture/RELEASE-POLICY.md"):
            self.write(path,"# Fixture\n`crates/medusa-core`\n")
        self.write("docs/architecture/INDEX.md",f"# Fixture\n{INDEX_SECTIONS}\n")
        self.write("docs/architecture/production-multi-agent-consolidation.md","# Certified production consolidation\n")
        self.write("docs/architecture/FINAL-CERTIFICATION-AUDIT.md","# Final audit\nNo unresolved deviation.\n")
        self.write("docs/architecture/decisions/0001-architecture-v2-reset.md","# ADR\n")
        self.write(".github/PULL_REQUEST_TEMPLATE.md",PR_TEXT); self.write(".github/CODEOWNERS",CODEOWNERS)
        self.write("scripts/check-architecture-index.py","# fixture\n"); self.write("scripts/test-architecture-index.py","# fixture\n"); self.write("scripts/architecture-conformance.py","# fixture\n")
        self.manifest=self.valid_manifest(); self.save_manifest()
    def close(self)->None: self.temp.cleanup()
    def write(self,relative:str,content:str)->None:
        path=self.root/relative; path.parent.mkdir(parents=True,exist_ok=True); path.write_text(content,encoding="utf-8")
    def write_cargo(self,members:list[str],extra_metadata:str="")->None:
        encoded=", ".join(json.dumps(x) for x in members)
        self.write("Cargo.toml",f'[workspace]\nresolver="2"\nmembers=[{encoded}]\n[workspace.metadata.medusa]\nproduction_entrypoint="{ENTRYPOINT}"\n{extra_metadata}')
    def save_manifest(self)->None: self.write("docs/architecture/baseline.json",json.dumps(self.manifest,indent=2))
    @staticmethod
    def valid_manifest()->dict[str,object]:
        migrations=[[issue,str(issue-646),f"phase {issue}","owner",["contract"],["consumer"],"completed: legacy removed"] for issue in range(646,656)]
        return {"schema_version":1,"baseline":{"issue":654,"parent_issue":645,"phase":"architecture-v2-final-certification","feature_freeze":{"active":False,"exceptions":[],"release_rule":"certified claims only"}},"deployment_modes":[["headless","medusa","crates/medusa-core","shared"]],"components":{"rust_crates":{"medusa-core":"preserve"},"non_crate":[["governance","docs/architecture","preserve"]],"owner_groups":{"foundation":["medusa-core"]}},"capabilities":[["core","production","certified-production","preserve","dispatcher",[]]],"capability_paths":{"core":["crates/medusa-core"]},"sources_of_truth":[["review and acceptance","dedicated zero-tool durable parent reviewer",[],"dedicated reviewer","generic sessions cannot accept mutations"],["session","journal",[],"aggregate","one authority"]],"state_machines":[["execution-production",EXECUTION,"review before integration"]],"known_failure_fixtures":[],"migration":migrations,"dependency_policy":{"forbidden_edges":[["crates/medusa-core","medusa-runtime"]]},"governance":{"index":"docs/architecture/INDEX.md","decision":"docs/architecture/decisions/0001-architecture-v2-reset.md","pr_template":".github/PULL_REQUEST_TEMPLATE.md","codeowners":".github/CODEOWNERS","checker":"scripts/check-architecture-index.py","conformance":"scripts/architecture-conformance.py","release_policy":"docs/architecture/RELEASE-POLICY.md","deletion_checklist":"docs/architecture/LEGACY-DELETION.md","final_audit":"docs/architecture/FINAL-CERTIFICATION-AUDIT.md"}}

class ArchitectureIndexTests(unittest.TestCase):
    def setUp(self)->None: self.fixture=Fixture()
    def tearDown(self)->None: self.fixture.close()
    def validate(self)->None: CHECKER.validate(self.fixture.root)
    def test_valid_fixture_passes(self)->None: self.validate()
    def test_new_workspace_crate_requires_index_entry(self)->None:
        self.fixture.write_cargo(["crates/medusa-core","crates/medusa-new"]); self.fixture.write("crates/medusa-new/Cargo.toml",'[package]\nname="medusa-new"\nversion="0.0.0"\n')
        with self.assertRaisesRegex(CHECKER.ArchitectureIndexError,"workspace/index crate mismatch"): self.validate()
    def test_new_entrypoint_requires_real_implementation(self)->None:
        self.fixture.manifest["deployment_modes"].append(["ghost","medusa ghost","crates/medusa-ghost","shared"]); self.fixture.save_manifest()
        with self.assertRaisesRegex(CHECKER.ArchitectureIndexError,"lacks implementation"): self.validate()
    def test_capability_requires_existing_production_path(self)->None:
        self.fixture.manifest["capabilities"].append(["ghost","withheld","quarantined","quarantine","missing",["gap"]]); self.fixture.manifest["capability_paths"]["ghost"]=["crates/medusa-ghost"]; self.fixture.save_manifest()
        with self.assertRaisesRegex(CHECKER.ArchitectureIndexError,"references missing implementation"): self.validate()
    def test_duplicate_authority_is_rejected(self)->None:
        self.fixture.manifest["sources_of_truth"].append(["workers","journal",[],"worker aggregate","one authority"]); self.fixture.save_manifest()
        with self.assertRaisesRegex(CHECKER.ArchitectureIndexError,"duplicate current authority"): self.validate()
    def test_forbidden_dependency_is_rejected(self)->None:
        self.fixture.write("crates/medusa-core/Cargo.toml",'[package]\nname="medusa-core"\nversion="0.0.0"\n[dependencies]\nmedusa-runtime="1"\n')
        with self.assertRaisesRegex(CHECKER.ArchitectureIndexError,"forbidden dependency present"): self.validate()
    def test_unknown_documented_component_is_rejected(self)->None:
        self.fixture.write("docs/CONTRIBUTOR-ARCHITECTURE.md","# Fixture\n`crates/medusa-does-not-exist`\n")
        with self.assertRaisesRegex(CHECKER.ArchitectureIndexError,"unknown crates/components"): self.validate()
    def test_active_feature_freeze_is_rejected(self)->None:
        self.fixture.manifest["baseline"]["feature_freeze"]["active"]=True; self.fixture.save_manifest()
        with self.assertRaisesRegex(CHECKER.ArchitectureIndexError,"inactive feature freeze"): self.validate()
    def test_generic_parent_agent_claim_is_rejected(self)->None:
        self.fixture.write_cargo(["crates/medusa-core"],'legacy_note="read-only parent medusa-agent::AgentEngine"\n')
        with self.assertRaisesRegex(CHECKER.ArchitectureIndexError,"stale final-certification claim"): self.validate()
    def test_post_integration_review_authority_is_rejected(self)->None:
        self.fixture.manifest["sources_of_truth"][0][1]="legacy parent AgentEngine after integration"; self.fixture.save_manifest()
        with self.assertRaisesRegex(CHECKER.ArchitectureIndexError,"dedicated durable reviewer"): self.validate()
    def test_duplicate_execution_state_machine_is_rejected(self)->None:
        self.fixture.manifest["state_machines"].append(["execution-current",["integrate","review"],"obsolete"]); self.fixture.save_manifest()
        with self.assertRaisesRegex(CHECKER.ArchitectureIndexError,"exactly one production execution state machine"): self.validate()
    def test_legacy_uncertified_status_is_rejected(self)->None:
        self.fixture.manifest["capabilities"][0][2]="legacy-uncertified"; self.fixture.save_manifest()
        with self.assertRaisesRegex(CHECKER.ArchitectureIndexError,"legacy-uncertified"): self.validate()
    def test_migration_component_disposition_is_rejected(self)->None:
        self.fixture.manifest["components"]["rust_crates"]["medusa-core"]="replace"; self.fixture.save_manifest()
        with self.assertRaisesRegex(CHECKER.ArchitectureIndexError,"migration component dispositions"): self.validate()

if __name__=="__main__": unittest.main()
