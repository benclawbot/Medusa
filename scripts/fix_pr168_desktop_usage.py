from pathlib import Path

path = Path("apps/medusa-desktop/src-tauri/src/dto.rs")
text = path.read_text()
old = """            RuntimeEvent::Usage {
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
            },
"""
new = """            RuntimeEvent::Usage {
                input_tokens,
                output_tokens,
                cache_read_input_tokens,
                cache_creation_input_tokens,
                duration_ms,
                ..
            } => Self::Usage {
                input_tokens,
                output_tokens,
                cache_read_input_tokens,
                cache_creation_input_tokens,
                model_elapsed_millis: duration_ms,
            },
"""
if text.count(old) != 1:
    raise SystemExit(f"expected one stale desktop usage adapter, found {text.count(old)}")
path.write_text(text.replace(old, new, 1))
