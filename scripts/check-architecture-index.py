#!/usr/bin/env python3
"""Validate Medusa's final Architecture v2 certification."""
from __future__ import annotations
import argparse, json, re, sys, tomllib
from pathlib import Path
from typing import Any

VALID_DISPOSITIONS={"preserve","adapt","replace","quarantine","delete"}
VALID_CERTIFICATIONS={"certified-production","quarantined","preview","experimental","design-only","deprecated"}
REQUIRED_MIGRATION_ISSUES=set(range(646,656)); REQUIRED_FIXTURES:set[str]=set()
REQUIRED_PR_TEXT={"## Architecture impact declaration","No architecture impact","Authority or source of truth","Versioned contracts or schemas","Trust/security boundary","Dependency direction","Production entrypoint or deployment mode","Capability lifecycle, readiness, permissions, or dispatcher","Legacy deletion target"}
REQUIRED_CODEOWNERS={"/docs/architecture/","/scripts/check-architecture-index.py","/scripts/architecture-conformance.py","/crates/medusa-runtime/","/crates/medusa-agent/","/crates/medusa-provider/","/crates/medusa-process-containment/","/crates/medusa-update/"}
REQUIRED_INDEX_SECTIONS={"## Final certification","## Certified production map","## Architecture contract","## Capability certification","## Source-of-truth matrix","## Dataflows","## Trust boundaries","## Known-failure compatibility fixtures","## Extension procedure","## Migration and deletion"}
MARKDOWN_LINK=re.compile(r"\[[^\]]+\]\(([^)]+)\)"); CRATE_REFERENCE=re.compile(r"(?<![A-Za-z0-9_-])crates/(medusa-[A-Za-z0-9_-]+)")
STALE={"read-only parent medusa-agent::AgentEngine","legacy parent AgentEngine after integration","integration precedes independent parent review"}
class ArchitectureIndexError(RuntimeError): pass

def read_text(root:Path, rel:str)->str:
    try: text=(root/rel).read_text(encoding="utf-8")
    except FileNotFoundError as exc: raise ArchitectureIndexError(f"missing required path: {rel}") from exc
    if not text.strip(): raise ArchitectureIndexError(f"empty required path: {rel}")
    return text

def load_json(root:Path, rel:str)->dict[str,Any]:
    try: value=json.loads(read_text(root,rel))
    except json.JSONDecodeError as exc: raise ArchitectureIndexError(f"invalid JSON in {rel}: {exc}") from exc
    if not isinstance(value,dict): raise ArchitectureIndexError(f"{rel} must contain a JSON object")
    return value

def unique(values:list[str], label:str)->None:
    duplicates={v for v in values if values.count(v)>1}
    if duplicates: raise ArchitectureIndexError(f"duplicate {label}: {sorted(duplicates)}")

def validate_workspace(root:Path,m:dict[str,Any])->set[str]:
    cargo=tomllib.loads(read_text(root,"Cargo.toml")); members=cargo.get("workspace",{}).get("members")
    if not isinstance(members,list): raise ArchitectureIndexError("Cargo.toml workspace.members must be a list")
    actual={x.removeprefix("crates/") for x in members if isinstance(x,str) and x.startswith("crates/")}
    indexed=m.get("components",{}).get("rust_crates")
    if not isinstance(indexed,dict): raise ArchitectureIndexError("components.rust_crates must be an object")
    if actual!=set(indexed): raise ArchitectureIndexError(f"workspace/index crate mismatch; missing={sorted(actual-set(indexed))}, unknown={sorted(set(indexed)-actual)}")
    invalid=[f"{k}:{v}" for k,v in indexed.items() if v not in VALID_DISPOSITIONS]
    if invalid: raise ArchitectureIndexError(f"invalid component dispositions: {invalid}")
    non_preserved=[k for k,v in indexed.items() if v!="preserve"]
    if non_preserved: raise ArchitectureIndexError(f"final certification retains migration component dispositions: {sorted(non_preserved)}")
    for name in indexed:
        if not (root/"crates"/name/"Cargo.toml").is_file(): raise ArchitectureIndexError(f"indexed crate has no Cargo.toml: {name}")
    owners=load_json(root,"docs/architecture/owners.json").get("owners")
    if not isinstance(owners,dict) or set(owners)!=actual: raise ArchitectureIndexError("primary owner registry drift")
    groups=m.get("components",{}).get("owner_groups")
    if not isinstance(groups,dict) or not groups: raise ArchitectureIndexError("components.owner_groups must be a non-empty object")
    refs={x for values in groups.values() if isinstance(values,list) for x in values if isinstance(x,str)}
    if refs-actual: raise ArchitectureIndexError(f"owner groups reference unknown crates: {sorted(refs-actual)}")
    return actual

def validate_components(root:Path,m:dict[str,Any])->None:
    rows=m.get("components",{}).get("non_crate")
    if not isinstance(rows,list): raise ArchitectureIndexError("components.non_crate must be a list")
    ids=[]
    for row in rows:
        if not isinstance(row,list) or len(row)!=3: raise ArchitectureIndexError(f"invalid non-crate component row: {row!r}")
        ident,path,disposition=row
        if disposition!="preserve": raise ArchitectureIndexError(f"final certification retains non-crate migration disposition: {row!r}")
        if not (root/path).exists(): raise ArchitectureIndexError(f"indexed component path does not exist: {path}")
        ids.append(ident)
    unique(ids,"non-crate component id")

def validate_deployment(root:Path,m:dict[str,Any])->None:
    rows=m.get("deployment_modes")
    if not isinstance(rows,list) or not rows: raise ArchitectureIndexError("deployment_modes must be a non-empty list")
    ids=[]
    for row in rows:
        if not isinstance(row,list) or len(row)!=4 or not all(isinstance(x,str) and x for x in row): raise ArchitectureIndexError(f"invalid deployment mode row: {row!r}")
        ident,_,path,_=row
        if not (root/path).exists(): raise ArchitectureIndexError(f"documented production entrypoint lacks implementation: {ident} -> {path}")
        ids.append(ident)
    unique(ids,"deployment mode id")

def validate_capabilities(root:Path,m:dict[str,Any])->None:
    rows=m.get("capabilities"); paths=m.get("capability_paths")
    if not isinstance(rows,list) or not isinstance(paths,dict): raise ArchitectureIndexError("capabilities and capability_paths must be populated")
    ids=[]
    for row in rows:
        if not isinstance(row,list) or len(row)!=6: raise ArchitectureIndexError(f"invalid capability row: {row!r}")
        ident,product,cert,disposition,dispatcher,gaps=row
        if not all(isinstance(x,str) and x for x in (ident,product,cert,disposition,dispatcher)): raise ArchitectureIndexError(f"invalid capability values: {row!r}")
        if cert not in VALID_CERTIFICATIONS or disposition not in VALID_DISPOSITIONS: raise ArchitectureIndexError(f"invalid capability lifecycle: {ident}")
        if not isinstance(gaps,list): raise ArchitectureIndexError(f"capability gaps must be a list: {ident}")
        if product=="production" and cert!="certified-production": raise ArchitectureIndexError(f"production capability is not certified-production: {ident}:{cert}")
        if cert=="certified-production" and gaps: raise ArchitectureIndexError(f"certified capability retains gaps: {ident}")
        impl=paths.get(ident)
        if not isinstance(impl,list) or not impl: raise ArchitectureIndexError(f"capability {ident} has no implementation paths")
        for path in impl:
            if not isinstance(path,str) or not (root/path).exists(): raise ArchitectureIndexError(f"capability {ident} references missing implementation: {path!r}")
        ids.append(ident)
    unique(ids,"capability id")
    if set(ids)!=set(paths): raise ArchitectureIndexError("capability_paths keys must exactly match capability ids")

def validate_lifecycle(m:dict[str,Any])->None:
    rows=m.get("sources_of_truth")
    if not isinstance(rows,list) or not rows: raise ArchitectureIndexError("sources_of_truth must be a non-empty list")
    concerns=[]; authorities=[]; review=""
    for row in rows:
        if not isinstance(row,list) or len(row)!=5: raise ArchitectureIndexError(f"invalid source-of-truth row: {row!r}")
        concern,authority,duplicates,target,invariant=row
        if not all(isinstance(x,str) and x for x in (concern,authority,target,invariant)) or not isinstance(duplicates,list): raise ArchitectureIndexError(f"invalid source-of-truth values: {row!r}")
        if duplicates: raise ArchitectureIndexError(f"final certification retains duplicate authorities: {concern}")
        if concern=="review and acceptance": review=authority
        concerns.append(concern); authorities.append(authority)
    unique(concerns,"source-of-truth concern"); unique(authorities,"current authority")
    if "dedicated" not in review or "reviewer" not in review: raise ArchitectureIndexError("review authority is not the dedicated durable reviewer")
    machines=m.get("state_machines")
    if not isinstance(machines,list): raise ArchitectureIndexError("state_machines must be a list")
    execution=[r for r in machines if isinstance(r,list) and len(r)==3 and isinstance(r[0],str) and r[0].startswith("execution")]
    if len(execution)!=1 or execution[0][0]!="execution-production": raise ArchitectureIndexError("final certification requires exactly one production execution state machine")
    states=execution[0][1]; required=["review-prepared-change","verify-independently","authorize","integrate-accepted-change","reconcile","persist-terminal-completion"]
    try: positions=[states.index(x) for x in required]
    except (ValueError,AttributeError) as exc: raise ArchitectureIndexError("production execution state machine lacks a required terminal gate") from exc
    if positions!=sorted(positions): raise ArchitectureIndexError("production execution state machine does not enforce review before integration")

def validate_migration(m:dict[str,Any])->None:
    if m.get("known_failure_fixtures")!=[]: raise ArchitectureIndexError("final architecture certification cannot retain compatibility fixtures")
    rows=m.get("migration")
    if not isinstance(rows,list): raise ArchitectureIndexError("migration must be a list")
    issues=[]
    for row in rows:
        if not isinstance(row,list) or len(row)!=7: raise ArchitectureIndexError(f"invalid migration row: {row!r}")
        issue,phase,goal,owner,contracts,consumers,deletion=row
        if not isinstance(issue,int) or not all(isinstance(x,str) and x for x in (phase,goal,owner,deletion)) or not isinstance(contracts,list) or not contracts or not isinstance(consumers,list) or not consumers: raise ArchitectureIndexError(f"invalid migration values: {row!r}")
        if not deletion.startswith("completed:"): raise ArchitectureIndexError(f"migration receipt is not completed: #{issue}")
        issues.append(issue)
    unique([str(x) for x in issues],"migration issue")
    if REQUIRED_MIGRATION_ISSUES-set(issues): raise ArchitectureIndexError(f"migration graph is missing issues: {sorted(REQUIRED_MIGRATION_ISSUES-set(issues))}")

def validate_dependencies(root:Path,m:dict[str,Any])->None:
    rows=m.get("dependency_policy",{}).get("forbidden_edges")
    if not isinstance(rows,list) or not rows: raise ArchitectureIndexError("dependency_policy.forbidden_edges must be non-empty")
    for source,target in rows:
        path=root/source/"Cargo.toml"
        if not path.is_file(): raise ArchitectureIndexError(f"forbidden-edge source does not exist: {source}")
        if re.search(rf"(?m)^\s*{re.escape(target.removeprefix('crates/'))}\s*=",path.read_text()): raise ArchitectureIndexError(f"forbidden dependency present: {source} -> {target.removeprefix('crates/')}")

def validate_governance(root:Path,m:dict[str,Any])->None:
    g=m.get("governance")
    if not isinstance(g,dict): raise ArchitectureIndexError("governance must be an object")
    for label,path in g.items():
        if not isinstance(path,str) or not (root/path).exists(): raise ArchitectureIndexError(f"governance path missing for {label}: {path!r}")
    index=read_text(root,g["index"])
    for section in REQUIRED_INDEX_SECTIONS:
        if section not in index: raise ArchitectureIndexError(f"architecture index is missing section: {section}")
    base=(root/g["index"]).parent
    for dest in MARKDOWN_LINK.findall(index):
        dest=dest.split("#",1)[0]
        if dest and "://" not in dest and not dest.startswith("mailto:") and not (base/dest).resolve().exists(): raise ArchitectureIndexError(f"stale architecture index link: {dest}")
    template=read_text(root,g["pr_template"]); codeowners=read_text(root,g["codeowners"])
    missing=sorted(x for x in REQUIRED_PR_TEXT if x not in template)
    if missing: raise ArchitectureIndexError(f"incomplete architecture declaration: {missing}")
    missing=sorted(x for x in REQUIRED_CODEOWNERS if x not in codeowners)
    if missing: raise ArchitectureIndexError(f"CODEOWNERS misses boundaries: {missing}")

def validate_documented(root:Path,known:set[str])->None:
    for rel in ["docs/ARCHITECTURE.md","docs/CONTRIBUTOR-ARCHITECTURE.md","docs/architecture/INDEX.md","docs/architecture/LEGACY-DELETION.md","docs/architecture/RELEASE-POLICY.md","docs/architecture/production-multi-agent-consolidation.md"]:
        unknown=sorted(set(CRATE_REFERENCE.findall(read_text(root,rel)))-known)
        if unknown: raise ArchitectureIndexError(f"{rel} references unknown crates/components: {unknown}")

def validate_final(root:Path,m:dict[str,Any])->None:
    b=m.get("baseline",{}); freeze=b.get("feature_freeze",{})
    if b.get("issue")!=654 or b.get("parent_issue")!=645: raise ArchitectureIndexError("final baseline must identify issues #654 and #645")
    if b.get("phase")!="architecture-v2-final-certification": raise ArchitectureIndexError("baseline phase is not final certification")
    if freeze.get("active") is not False or freeze.get("exceptions")!=[] or not freeze.get("release_rule"): raise ArchitectureIndexError("final certification requires an inactive feature freeze and release rule")
    if "legacy-uncertified" in json.dumps(m,sort_keys=True): raise ArchitectureIndexError("final certification retains legacy-uncertified status")
    cargo=tomllib.loads(read_text(root,"Cargo.toml")); entry=cargo.get("workspace",{}).get("metadata",{}).get("medusa",{}).get("production_entrypoint","")
    order=["dedicated durable parent reviewer","independent verification","authorization","integration","reconciliation","canonical terminal persistence"]
    positions=[entry.find(x) for x in order]
    if any(x<0 for x in positions) or positions!=sorted(positions): raise ArchitectureIndexError("workspace metadata does not describe review-before-integration execution")
    combined="\n".join(read_text(root,x) for x in ["Cargo.toml","docs/architecture/INDEX.md","docs/architecture/production-multi-agent-consolidation.md"])
    for stale in STALE:
        if stale in combined: raise ArchitectureIndexError(f"stale final-certification claim reintroduced: {stale}")
    if not (root/"crates/medusa-runtime/src/mutation_transaction_state.rs").is_file() or (root/"crates/medusa-runtime/src/mutation_transaction_legacy.rs").exists(): raise ArchitectureIndexError("authoritative mutation state module or legacy deletion receipt drifted")

def validate(root:Path,manifest_relative:str="docs/architecture/baseline.json")->None:
    m=load_json(root,manifest_relative)
    if m.get("schema_version")!=1: raise ArchitectureIndexError("unsupported architecture baseline schema_version")
    validate_final(root,m); known=validate_workspace(root,m); validate_components(root,m); validate_deployment(root,m); validate_capabilities(root,m); validate_lifecycle(m); validate_migration(m); validate_dependencies(root,m); validate_governance(root,m); validate_documented(root,known)

def main()->int:
    p=argparse.ArgumentParser(); p.add_argument("--root",type=Path,default=Path(".")); p.add_argument("--manifest",default="docs/architecture/baseline.json"); a=p.parse_args()
    try: validate(a.root.resolve(),a.manifest)
    except ArchitectureIndexError as exc: print(f"architecture-index-error: {exc}",file=sys.stderr); return 1
    print("architecture-index-ok"); return 0
if __name__=="__main__": raise SystemExit(main())
