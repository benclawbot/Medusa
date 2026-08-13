# Live self-improvement acceptance

Medusa's production self-improvement loop has a protected live acceptance gate in
`.github/workflows/live-self-improvement.yml`. The gate uses the configured MiniMax route and the
same correction processing, authority review, runtime selection, outcome attribution, privacy, and
rollback services used by the product.

The acceptance run starts with a real model response and a user correction. Session completion must
produce an evaluated candidate without editing authority files. The run then approves and activates
the candidate through the production review service, applies it to two later live model requests,
records two directly attributed verified outcomes, proves that a nonmatching task receives no learned
context, and rolls the candidate back. The active authority projection after rollback must hash to the
exact same value as the projection before activation. A final privacy probe disables capture and
proves that another correction cannot create a candidate.

The uploaded `live-self-improvement-evidence/report.json` is deliberately sanitized. It contains the
provider and model identifiers, commit and platform, lifecycle booleans, counts, and SHA-256 digests
with character counts for model and user text. It never contains the live credential, raw model
responses, or correction text. The temporary repository that contains the production journals is not
uploaded.

Maintainers can run the gate from GitHub Actions with the protected `MINIMAX_API_KEY` secret, or run
the ignored test in a disposable environment:

```text
MEDUSA_LIVE_SELF_IMPROVEMENT_REPORT=<output-path> \
MINIMAX_API_KEY=<session-secret> \
cargo test -p medusa-runtime --test live_self_improvement_loop --locked -- --ignored --nocapture
```

Ordinary workspace test runs compile but do not execute this networked acceptance test. Pull requests
with the `final-issue-validation` label execute it as a required live product gate and upload the
sanitized report for review.
