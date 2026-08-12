# External repository validation

Deterministic product-acceptance gates remain authoritative. This programme adds repeatable evidence from pinned public repositories without replacing those gates or claiming benchmark generality.

## Contracts and corpus

Scenario manifests live in `validation/external-repositories/scenarios/`. Each records a full repository commit, task, expected invariants, allowed scope, provider/configuration, timeout, acceptance criteria, and covered failure modes. The initial corpus covers five repositories and at least three language/build ecosystems, including interruption/resume, failed verification, rollback, transient provider failure, and a run budget longer than one hour.

Reports conform to `validation/external-repositories/report.schema.json` and include the exact Medusa commit, repository commit, platform, provider metadata, scenario version, elapsed time, execution steps, repository changes, evidence validation failures, and completion/recovery metrics.

## Required CI validation

```bash
python3 scripts/external-validation.py validate
python3 scripts/external-validation.py self-test
```

These commands are offline and validate manifests, corpus coverage, timeout reporting, replay-adapter requirements, and report round-tripping.

## Execution adapter contract

The runner does not pretend that the normal interactive Medusa CLI is a deterministic replay provider. The command supplied to `--medusa-command` must be an execution adapter that:

- accepts the scenario objective through `{task}` or `{task_file}`;
- accepts `{evidence_file}` and writes a JSON evidence record there;
- accepts `{provider_fixture}` for every offline scenario and actually configures the recorded-replay provider;
- exits non-zero when execution itself fails.

The evidence JSON must contain:

```json
{
  "outcome": "verified-completion-after-resume",
  "completion_claimed": true,
  "evidence": ["checkpoint", "resume-event", "verification-result", "repository-diff"],
  "metrics": {
    "intervention_count": 0,
    "recovery_outcome": "resumed-and-verified",
    "policy_denials": 0,
    "unrecovered_state": false
  }
}
```

The runner independently checks verification commands, repository changes, and `allowed_scope`. A zero exit code without the required evidence cannot produce a passing report.

## Reproducing a non-live scenario

From a clean Medusa checkout, install dependencies for the pinned target repository and provide a replay adapter that implements the contract above:

```bash
python3 scripts/external-validation.py run ripgrep-interruption-resume \
  --output target/external-validation \
  --medusa-command python3 scripts/run-recorded-medusa.py \
    --repo {repo} \
    --task-file {task_file} \
    --fixture {provider_fixture} \
    --evidence-file {evidence_file}
```

`run-recorded-medusa.py` is an example name for a separately supplied adapter; the harness deliberately refuses to label an ordinary provider invocation as offline replay. The adapter must invoke Medusa with a valid non-interactive approval allowlist and pass the task contents as the objective rather than relying on unsupported response-file expansion.

The command clones the repository, checks out the exact commit, runs the adapter, executes the manifest's verification commands, validates evidence and changed paths, and writes machine-readable and human-readable reports. A command timeout is recorded as a failed step and still produces both reports. Networked checkout and expensive execution are never part of required pull-request CI.

Live-provider scenarios require `MEDUSA_EXTERNAL_LIVE=1`. Never store credentials in manifests or committed artifacts. Failed-run evidence must be reviewed before publication, and public summaries must state limitations explicitly.
