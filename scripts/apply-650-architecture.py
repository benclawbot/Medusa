#!/usr/bin/env python3
from __future__ import annotations

import json
from pathlib import Path

root = Path('.')

# Machine-readable ownership and migration baseline.
owners_path = root / 'docs/architecture/owners.json'
owners = json.loads(owners_path.read_text(encoding='utf-8'))
owners['owners']['medusa-evidence'] = 'evidence'
owners['owners'] = dict(sorted(owners['owners'].items()))
owners_path.write_text(json.dumps(owners, indent=2) + '\n', encoding='utf-8')

baseline_path = root / 'docs/architecture/baseline.json'
baseline = json.loads(baseline_path.read_text(encoding='utf-8'))
components = baseline['components']
components['rust_crates']['medusa-evidence'] = 'preserve'
components['rust_crates'] = dict(sorted(components['rust_crates'].items()))
components['owner_groups']['evidence'] = ['medusa-evidence']

capability_id = 'evidence-verification-authority'
capability = [
    capability_id,
    'legacy-free-form',
    'certified-production',
    'preserve',
    'medusa-evidence::VerificationPlanner and VerificationReceipt',
    [],
]
baseline['capabilities'] = [
    row for row in baseline['capabilities'] if row[0] != capability_id
] + [capability]
baseline['capability_paths'][capability_id] = [
    'crates/medusa-evidence',
    'crates/medusa-agent/src/verification_authority.rs',
    'crates/medusa-runtime/src/mutation_transaction.rs',
    'crates/medusa-workers/src/lib.rs',
]

for row in baseline['capabilities']:
    if row[0] == 'multi-agent-research':
        row[2] = 'legacy-uncertified'
        row[5] = [
            gap
            for gap in row[5]
            if gap
            not in {
                'integration precedes parent review',
                'task/reviewer state is partly decorative',
                'isolated verification does not receive changed paths',
            }
        ]

for row in baseline['sources_of_truth']:
    if row[0] == 'verification':
        row[1] = 'medusa-evidence VerificationPlan and VerificationReceipt'
        row[2] = ['human summaries', 'legacy coarse repository result']
        row[3] = 'typed changed-component verification authority'
        row[4] = 'every required selected check has a source-bound receipt for the exact commit and scope'
    elif row[0] == 'evidence and artifacts':
        row[1] = 'medusa-evidence EvidenceBundle and content-addressed ArtifactStore'
        row[2] = ['human summaries']
        row[3] = 'typed source-bound evidence and durable content-addressed artifacts'
        row[4] = 'verified conclusions resolve exact sources and durable read receipts'

baseline['known_failure_fixtures'] = [
    row
    for row in baseline['known_failure_fixtures']
    if row[0] != 'isolated-verification-drops-changed-paths'
]
for row in baseline['migration']:
    if row[0] == 650:
        row[:] = [
            650,
            '4',
            'authoritative evidence, artifacts, and changed-component verification',
            'evidence',
            [
                'EvidenceRecord',
                'EvidenceBundle',
                'ArtifactStore',
                'ChangedComponent',
                'VerificationPlan',
                'VerificationReceipt',
            ],
            [
                'medusa-agent',
                'medusa-runtime',
                'medusa-workers',
                'medusa-multi-agent-scheduler',
            ],
            'free-form evidence strings, name-only scope, and coarse verification results',
        ]
        break
else:
    raise SystemExit('missing #650 migration row')

baseline_path.write_text(json.dumps(baseline, indent=2) + '\n', encoding='utf-8')

# Human-readable living index.
index_path = root / 'docs/architecture/INDEX.md'
index = index_path.read_text(encoding='utf-8')
old_current = '''```mermaid
flowchart LR
  UI[TUI / CLI / Desktop / Daemon] --> R[RuntimeController]
  R --> P[Production orchestrator]
  P --> RO[Read-only planner and risk reviewer]
  P --> MW[Mutating worktree coordinator]
  MW --> WV[Worktree verification]
  WV --> I[Primary-tree integration]
  I --> PR[Parent read-only review]
  PR --> RV[Repository verification]
  R --> J[(Session journal and .medusa records)]
  RO --> J
  MW --> J
  RV --> J
```

This is an inventory, not the desired architecture. Known defects include integration before independent review, verification that does not receive changed paths, advertised browser tools without production dispatch, and provider capability claims that do not match wire or cancellation behavior.
'''
new_current = '''```mermaid
flowchart LR
  UI[TUI / CLI / Desktop / Daemon] --> R[RuntimeController]
  R --> P[Production orchestrator]
  P --> RO[Read-only planner and risk reviewer]
  P --> MW[Mutating worktree coordinator]
  MW --> WC[Exact changed-component scope]
  WC --> WV[Typed worktree verification receipt]
  WV --> PR[Parent review of immutable prepared commit]
  PR --> IV[Independent typed verification receipt]
  IV --> I[Authorized primary-tree integration]
  I --> RC[Reconciliation]
  WV --> E[(EvidenceBundle / ArtifactStore)]
  IV --> E
  R --> J[(Session journal and transaction records)]
```

This is the current migration state. Transactional review-before-integration and authoritative evidence are production paths; remaining known defects are tracked in later phases, including provider wire/cancellation parity and shared frontend certification.
'''
if old_current not in index:
    raise SystemExit('current architecture map anchor drifted')
index = index.replace(old_current, new_current, 1)
old_target = '''```mermaid
flowchart LR
  S[Versioned frontend commands] --> O[Single orchestration core]
  O --> PA[(Plan aggregate)]
  PA --> W[Leased isolated worker]
  W --> V[Changed-path-aware verification]
  V --> R[Independent prepared-change review]
  R -->|accepted receipt| I[Single mutation and integration service]
  I --> PV[Primary repository verification]
  PV --> E[(Versioned evidence and artifact envelope)]
  O --> C[Generated capability registry]
  C --> D[Certified dispatchers and permission gates]
  O --> H[Durable provider route health]
```
'''
new_target = '''```mermaid
flowchart LR
  S[Versioned frontend commands] --> O[Single orchestration core]
  O --> PA[(Plan aggregate)]
  PA --> W[Leased isolated worker]
  W --> C[Exact ChangedComponent scope]
  C --> VP[VerificationPlanner]
  VP --> VR[(Typed VerificationReceipt)]
  VR --> R[Independent prepared-change review]
  R -->|accepted receipt| I[Single mutation and integration service]
  I --> RC[Reconciliation]
  VP --> A[(Content-addressed ArtifactStore)]
  A --> EB[(Source-bound EvidenceBundle)]
  O --> CR[Generated capability registry]
  CR --> D[Certified dispatchers and permission gates]
  O --> H[Durable provider route health]
```
'''
if old_target not in index:
    raise SystemExit('target architecture map anchor drifted')
index = index.replace(old_target, new_target, 1)
index = index.replace(
    '- changed paths remain explicit through implementation, verification, review, integration, and evidence;\n',
    '- additions, modifications, renames, deletions, generated files, ownership, and effective UI impact remain explicit through implementation, verification, review, integration, and evidence;\n- verified conclusions resolve typed sources and prove the artifact ranges actually read;\n',
    1,
)
capability_anchor = '| Identity, approvals, transactions | production | legacy-uncertified | adapt | centralize mutation receipts and authority |\n'
capability_row = capability_anchor + '| Evidence, artifacts, verification | legacy-free-form | certified-production | preserve | typed source-bound receipts and content-addressed artifacts are authoritative |\n'
if capability_anchor not in index:
    raise SystemExit('capability table anchor drifted')
index = index.replace(capability_anchor, capability_row, 1)
index = index.replace(
    '| Verification | repository gate and targeted checks | changed-path-aware receipt | changed paths survive every transition |\n',
    '| Verification | `medusa-evidence::VerificationPlan` and `VerificationReceipt` | typed changed-component authority | every required check is bound to the exact commit, scope, command outputs, and artifacts |\n',
    1,
)
index = index.replace(
    '| Evidence/artifacts | `.medusa` records and release evidence | versioned envelope | reports derive from evidence |\n',
    '| Evidence/artifacts | `EvidenceBundle` and content-addressed `ArtifactStore` | typed source-bound envelope | conclusions resolve exact sources and durable read receipts |\n',
    1,
)
index = index.replace(
    '- **Evidence:** command, worker, verification, review, integration, recovery, and artifact receipts → versioned evidence envelope → report/UI/release consumers.\n',
    '- **Evidence:** exact changed components → selected checks → raw command/browser/artifact outputs → content-addressed artifacts and read receipts → typed claims/decisions → review, scheduler, authorization, integration, report, and UI consumers.\n',
    1,
)
index = index.replace('- `isolated-verification-drops-changed-paths` (#633)\n', '', 1)
index = index.replace(
    '| 4 | #650 | provider/OAuth route authority |\n',
    '| 4 | #650 | authoritative evidence, artifacts, and changed-component verification |\n',
    1,
)
decision_anchor = '- Decision: [`decisions/0005-transactional-mutation-lifecycle.md`](decisions/0005-transactional-mutation-lifecycle.md)\n'
if decision_anchor not in index:
    raise SystemExit('decision index anchor drifted')
index = index.replace(
    decision_anchor,
    decision_anchor + '- Decision: [`decisions/0006-authoritative-evidence-artifacts-and-verification.md`](decisions/0006-authoritative-evidence-artifacts-and-verification.md)\n',
    1,
)
index_path.write_text(index, encoding='utf-8')

# Delete the solved known-failure probe and checker requirement.
checker_path = root / 'scripts/check-architecture-index.py'
checker = checker_path.read_text(encoding='utf-8')
checker = checker.replace(
    '''REQUIRED_FIXTURES = {
    "isolated-verification-drops-changed-paths",
    "provider-capability-mismatch",
}
''',
    '''REQUIRED_FIXTURES = {
    "provider-capability-mismatch",
}
''',
    1,
)
checker_path.write_text(checker, encoding='utf-8')

conformance_path = root / 'scripts/architecture-conformance.py'
conformance = conformance_path.read_text(encoding='utf-8')
start = conformance.index('def verification_drops_changed_paths(')
end = conformance.index('\ndef provider_capability_mismatch(', start)
conformance = conformance[:start] + conformance[end + 1:]
conformance = conformance.replace(
    '    "isolated-verification-drops-changed-paths": verification_drops_changed_paths,\n',
    '',
    1,
)
conformance_path.write_text(conformance, encoding='utf-8')

# Permanent semantic guard for the authority path.
semantic_path = root / 'scripts/check-evidence-authority.py'
semantic_path.write_text('''#!/usr/bin/env python3
from pathlib import Path

root = Path(__file__).resolve().parents[1]

def read(relative: str) -> str:
    return (root / relative).read_text(encoding="utf-8")

checks = {
    "typed evidence crate": "pub struct EvidenceRecord" in read("crates/medusa-evidence/src/evidence.rs"),
    "content-addressed artifact store": "pub struct ArtifactStore" in read("crates/medusa-evidence/src/artifact.rs"),
    "durable read receipts": "ArtifactReadReceipt" in read("crates/medusa-evidence/src/artifact.rs"),
    "exact changed components": "pub struct ChangedComponent" in read("crates/medusa-evidence/src/change.rs"),
    "planner selects browser behavior": "BrowserBehavior" in read("crates/medusa-evidence/src/verification.rs"),
    "planner selects accessibility": "Accessibility" in read("crates/medusa-evidence/src/verification.rs"),
    "command outputs become receipts": "CommandReceipt::new" in read("crates/medusa-agent/src/verification_authority.rs"),
    "browser verification is mandatory": "required_browser_verification" in read("crates/medusa-agent/src/verification.rs"),
    "accessibility behavior is inspected": "unlabeled_controls" in read("crates/medusa-agent/src/verification.rs"),
    "worker preserves git change kinds": "commit_changed_components" in read("crates/medusa-workers/src/lib.rs"),
    "isolated implementation uses authority": "authoritative_verification_for_components_at" in read("crates/medusa-runtime/src/mutating_worker_coordinator.rs"),
    "independent verification uses authority": "authoritative_verification_for_components_at" in read("crates/medusa-runtime/src/mutation_transaction.rs"),
    "scheduler validates evidence dependencies": "succeed_with_evidence" in read("crates/medusa-multi-agent-scheduler/src/lib.rs"),
    "coarse verifier deleted": "targeted_verification" not in read("crates/medusa-agent/src/verification.rs"),
    "changed-path-loss fixture deleted": "isolated-verification-drops-changed-paths" not in read("scripts/architecture-conformance.py"),
}
failed = [name for name, passed in checks.items() if not passed]
for name, passed in checks.items():
    print(f"{'passed' if passed else 'failed'}: {name}")
if failed:
    raise SystemExit(f"evidence authority drift: {failed}")
''', encoding='utf-8')

# Run semantic authority validation in the cross-platform architecture baseline.
workflow_path = root / '.github/workflows/architecture-v2-baseline.yml'
workflow = workflow_path.read_text(encoding='utf-8')
workflow = workflow.replace(
    "      - 'scripts/check-mutation-lifecycle.py'\n",
    "      - 'scripts/check-mutation-lifecycle.py'\n      - 'scripts/check-evidence-authority.py'\n",
    2,
)
step = '''      - name: Validate transactional mutation ordering
        run: python scripts/check-mutation-lifecycle.py
'''
if step not in workflow:
    raise SystemExit('architecture workflow step anchor drifted')
workflow = workflow.replace(
    step,
    step + '''      - name: Validate evidence and verification authority
        run: python scripts/check-evidence-authority.py
''',
    1,
)
workflow_path.write_text(workflow, encoding='utf-8')
