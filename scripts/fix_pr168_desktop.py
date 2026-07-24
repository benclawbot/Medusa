from pathlib import Path

path = Path("apps/medusa-desktop/src-tauri/src/dto.rs")
text = path.read_text()
text = text.replace(
'''    Usage {
        input_tokens: u64,
        output_tokens: u64,
        cache_read_input_tokens: u64,
        cache_creation_input_tokens: u64,
        model_elapsed_millis: u64,
    },''',
'''    Usage {
        input_tokens: u64,
        output_tokens: u64,
        cache_read_input_tokens: u64,
        cache_creation_input_tokens: u64,
        total_tokens: u64,
        duration_ms: u64,
        tokens_per_second_milli: u64,
        estimated_cost_microusd: u64,
        provenance: String,
    },''',
1,
)
text = text.replace(
'''            RuntimeEvent::Usage {
                input_tokens,
                output_tokens,
                cache_read_input_tokens,
                cache_creation_input_tokens,
                model_elapsed_millis,
            } => Self::Usage {
                input_tokens,
                output_tokens,
                cache_read_input_tokens,
                cache_creation_input_tokens,
                model_elapsed_millis,
            },''',
'''            RuntimeEvent::Usage {
                input_tokens,
                output_tokens,
                cache_read_input_tokens,
                cache_creation_input_tokens,
                total_tokens,
                duration_ms,
                tokens_per_second_milli,
                estimated_cost_microusd,
                provenance,
            } => Self::Usage {
                input_tokens,
                output_tokens,
                cache_read_input_tokens,
                cache_creation_input_tokens,
                total_tokens,
                duration_ms,
                tokens_per_second_milli,
                estimated_cost_microusd,
                provenance: format!("{provenance:?}").to_ascii_lowercase(),
            },''',
1,
)
if "model_elapsed_millis" in text:
    raise SystemExit("stale model_elapsed_millis remains in desktop DTO")
path.write_text(text)
