from pathlib import Path

path = Path('crates/medusa-provider/src/route_metrics_store.rs')
text = path.read_text()
old = '''            stats.output_tokens = stats.output_tokens.saturating_add(usage.output_tokens);
            let generation_ms = duration_ms.saturating_sub(first_token_ms.unwrap_or_default());
            stats.generation_total_ms = stats.generation_total_ms.saturating_add(generation_ms);'''
new = '''            if let Some(first_token_ms) = first_token_ms {
                stats.output_tokens = stats.output_tokens.saturating_add(usage.output_tokens);
                let generation_ms = duration_ms.saturating_sub(first_token_ms);
                stats.generation_total_ms = stats.generation_total_ms.saturating_add(generation_ms);
            }'''
if old not in text:
    raise SystemExit('throughput fragment not found')
path.write_text(text.replace(old, new, 1))

old_test = '''        assert_eq!(stats.output_tokens, 0);
        assert_eq!(stats.generation_total_ms, 120);'''
new_test = '''        assert_eq!(stats.output_tokens, 0);
        assert_eq!(stats.generation_total_ms, 0);'''
if old_test not in path.read_text():
    raise SystemExit('test fragment not found')
path.write_text(path.read_text().replace(old_test, new_test, 1))
