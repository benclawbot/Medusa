# Usage telemetry

Medusa records normalized model usage in the durable session event stream. Each successful model turn includes input, output, cache-read, cache-write, and total tokens; measured request duration; tokens per second; estimated cost; and whether token counts were provider-reported or deterministically estimated.

The TUI consumes this normalized event directly. It does not independently time requests or reconstruct partial provider usage, so the displayed values match the durable session record.

## Cost configuration

Cost rates use integer micro-USD per million tokens:

- `MEDUSA_INPUT_COST_MICROUSD_PER_MILLION`
- `MEDUSA_OUTPUT_COST_MICROUSD_PER_MILLION`
- `MEDUSA_CACHE_READ_COST_MICROUSD_PER_MILLION`
- `MEDUSA_CACHE_WRITE_COST_MICROUSD_PER_MILLION`

Unset rates default to zero. The TUI displays an em dash when cumulative estimated cost is zero.

## Provenance

`provider` means the configured provider supplied token counts. `estimated` means Medusa used its deterministic fallback estimator because the provider returned no usage counts.
