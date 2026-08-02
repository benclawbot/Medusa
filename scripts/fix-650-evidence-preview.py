#!/usr/bin/env python3
from pathlib import Path

path = Path('crates/medusa-agent/src/verification_authority.rs')
text = path.read_text(encoding='utf-8')

import_anchor = 'use sha2::{Digest, Sha256};\n'
constants = '''use sha2::{Digest, Sha256};

const COMMAND_PREVIEW_MAX_BYTES: usize = 4 * 1024;
const COMMAND_PREVIEW_MAX_LINES: usize = 32;
'''
if text.count(import_anchor) != 1:
    raise SystemExit(f'import anchor count={text.count(import_anchor)}')
text = text.replace(import_anchor, constants, 1)

materialize_start = text.index('fn materialize_command(')
command_start = text.index('    let command = CommandReceipt::new(', materialize_start)
preview = '''    let mut preview_details = command_output_preview("stdout", &executed.stdout);
    preview_details.extend(command_output_preview("stderr", &executed.stderr));
'''
text = text[:command_start] + preview + text[command_start:]

details_start = text.index('    let details = vec![', command_start)
text = text[:details_start] + text[details_start:].replace(
    '    let details = vec![', '    let mut details = vec![', 1
)
details_end = text.index('    ];', details_start) + len('    ];')
text = text[:details_end] + '\n    details.extend(preview_details);' + text[details_end:]

helper_anchor = 'fn rejected_material(reason: String) -> CheckMaterial {\n'
helper = '''fn command_output_preview(stream: &str, bytes: &[u8]) -> Vec<String> {
    if bytes.is_empty() {
        return Vec::new();
    }
    let text = String::from_utf8_lossy(bytes);
    let mut lines = Vec::new();
    let mut remaining = COMMAND_PREVIEW_MAX_BYTES;
    let mut source_lines = text.lines();
    for source in source_lines.by_ref().take(COMMAND_PREVIEW_MAX_LINES) {
        if remaining == 0 {
            break;
        }
        let normalized = source
            .chars()
            .map(|character| {
                if character == '\\t' {
                    ' '
                } else if character.is_control() {
                    '�'
                } else {
                    character
                }
            })
            .collect::<String>();
        let preview = if secret_like(&normalized) {
            "[redacted secret-like output]".to_owned()
        } else {
            truncate_utf8(&normalized, remaining)
        };
        remaining = remaining.saturating_sub(preview.len());
        lines.push(format!(
            "command_{stream}_preview_non_authoritative={preview}"
        ));
    }
    if source_lines.next().is_some()
        || text.len() > COMMAND_PREVIEW_MAX_BYTES
        || lines.len() == COMMAND_PREVIEW_MAX_LINES
    {
        lines.push(format!(
            "command_{stream}_preview_non_authoritative=[truncated; full output is stored as a content-addressed artifact]"
        ));
    }
    lines
}

fn truncate_utf8(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_owned();
    }
    let mut end = max_bytes;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_owned()
}

fn secret_like(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    [
        "api_key",
        "apikey",
        "authorization:",
        "bearer ",
        "password=",
        "secret=",
        "token=",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
}

''' + helper_anchor
if text.count(helper_anchor) != 1:
    raise SystemExit(f'helper anchor count={text.count(helper_anchor)}')
text = text.replace(helper_anchor, helper, 1)

test_anchor = '''    #[test]
    fn corrupt_nonempty_json_fails_authoritative_receipt() {
'''
tests = '''    #[test]
    fn command_preview_is_bounded_and_redacts_secret_like_lines() {
        let long = "x".repeat(COMMAND_PREVIEW_MAX_BYTES + 512);
        let output = format!(
            "verified-value-42\\ntoken=do-not-persist\\n{long}\\nextra-line"
        );
        let preview = command_output_preview("stdout", output.as_bytes());
        assert!(preview.iter().any(|line| line.contains("verified-value-42")));
        assert!(
            preview
                .iter()
                .any(|line| line.contains("[redacted secret-like output]"))
        );
        assert!(!preview.iter().any(|line| line.contains("do-not-persist")));
        assert!(preview.iter().any(|line| line.contains("[truncated;")));
        assert!(
            preview.iter().map(String::len).sum::<usize>()
                < COMMAND_PREVIEW_MAX_BYTES + 2 * 1024
        );
    }

''' + test_anchor
if text.count(test_anchor) != 1:
    raise SystemExit(f'test anchor count={text.count(test_anchor)}')
path.write_text(text.replace(test_anchor, tests, 1), encoding='utf-8')
