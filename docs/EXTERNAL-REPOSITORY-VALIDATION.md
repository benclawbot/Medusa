# External repository validation

Deterministic product-acceptance gates remain authoritative. This programme adds repeatable evidence from pinned public repositories without replacing those gates or claiming benchmark generality.

## Contracts and corpus

Scenario manifests live in `validation/external-repositories/scenarios/`. Each records a full repository commit, task, expected invariants, allowed scope, provider/configuration, timeout, acceptance criteria, and covered failure modes. The initial corpus covers five repositories and at least three language/build ecosystems, including interruption/resume, failed verification, rollback, transient provider failure, and a run budget longer than one hour.

Reports conform to `validation/external-repositories/report.schema.json` and include the exact Medusa commit, repository commit, platform, provider metadata, scenario version, elapsed time, execution steps, and completion/recovery metrics.

## Required CI validation

```bash
python3 scripts/external-validation.py validate
python3 scripts/external-validation.py self-test
```

These commands are offline and validate manifests, corpus coverage, and report round-tripping.

## Reproducing a non-live scenario

From a clean Medusa checkout, install dependencies for the pinned target repository and provide a recorded-replay provider command:

```bash
python3 scripts/external-validation.py run ripgrep-interruption-resume \
  --output target/external-validation \
  --medusa-command medusa --repo {repo} run --non-interactive @\{task_file\}
```

The command clones the repository, checks out the exact commit, runs Medusa, executes the manifest's verification commands, and writes machine-readable and human-readable reports. Networked checkout and expensive execution are never part of required pull-request CI.

Live-provider scenarios require `MEDUSA_EXTERNAL_LIVE=1`. Never store credentials in manifests or committed artifacts. Failed-run evidence must be reviewed before publication, and public summaries must state limitations explicitly.
