# Live-provider dogfood

The live-provider dogfood gate exercises the public `medusa` executable against a disposable multi-language repository on Linux, macOS, and Windows. It complements deterministic pull-request acceptance; provider availability is not a normal PR requirement.

The selected provider, model, protocol, endpoint, authentication mode, and credential environment come from the single `primary` entry in `docs/provider-support.json`. The harness does not maintain an independent provider declaration.

## Execution contract

Each platform job builds the release binary, copies it into an isolated staged installation, launches it from an unrelated working directory with an explicit `--repo`, and asks one production MiniMax-M3 session to repair three independent defects. The agent cannot modify the verifier, JavaScript test, package contract, fixtures, or expected outputs.

The harness then:

1. verifies the protected contract is byte-identical;
2. runs the independent verifier;
3. reads durable model usage from the session event stream;
4. captures the repository patch, status, session evidence, and a sanitized log;
5. proves the credential is absent from the isolated home, fixture, and retained artifacts;
6. records the exact commit and staged executable SHA-256.

## Protection budgets

The run fails closed when any limit is exceeded:

- wall clock: 1,500 seconds;
- model turns: 16 per worker;
- concurrency: 2 workers;
- context window: 32,768 tokens;
- output: 4,096 tokens per request;
- provider retries: 2;
- deterministic accounting ceiling: 20,000,000 micro-USD.

Cost evidence uses deliberately conservative accounting rates configured only for the test: 5,000,000 micro-USD per million input/cache tokens and 20,000,000 per million output tokens. The theoretical maximum for the configured turn, worker, context, and output limits must fit below the declared ceiling before the provider is called. Actual normalized usage is then reconstructed from durable `model_response_received` events and checked again.

## Credential boundary

`MINIMAX_API_KEY` is provided only through the job environment. Checkout credentials are not persisted. The harness redirects home/config/state directories into a disposable root, redacts the exact secret from captured output, scans every retained file for the raw credential, and deletes evidence if that audit cannot complete successfully.

## Evidence and failure classes

Every platform emits a versioned `summary.json` plus sanitized diagnostic evidence. The aggregate report requires Linux, macOS, and Windows results from one exact commit and accepts only these failure classes:

- `product`;
- `provider`;
- `environment`;
- `flaky-test`.

The report records platform/build identity, assertion results, elapsed time, normalized token/cost usage, failure details, and known limitations. A missing platform, mismatched commit, incomplete credential audit, changed verification contract, invalid budget, or missing executable identity fails the report.

## Running

The workflow supports manual dispatch and a weekly schedule. Its live matrix also runs on the dedicated implementation PR when the harness contract changes, while unrelated pull requests run only deterministic product acceptance.

Locally, with the provider credential set:

```text
python scripts/live-coding-e2e.py --output live-e2e-artifacts
```

Contract-only tests do not require a provider credential:

```text
python scripts/test-live-dogfood.py
```
