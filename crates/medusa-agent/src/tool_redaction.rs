const REDACTED: &str = "[REDACTED]";

const ASSIGNMENT_NAMES: &[&str] = &[
    "authorization",
    "api_key",
    "api-key",
    "apikey",
    "access_token",
    "access-token",
    "token",
    "password",
    "passwd",
    "secret",
    "client_secret",
    "client-secret",
    "x-api-key",
    "x_amz_signature",
    "x-amz-signature",
    "x-amz-credential",
    "signature",
    "sig",
];

const CLI_FLAGS: &[&str] = &[
    "--authorization",
    "--api-key",
    "--apikey",
    "--access-token",
    "--token",
    "--password",
    "--passwd",
    "--secret",
    "--client-secret",
];

const TOKEN_PREFIXES: &[&str] = &["github_pat_", "ghp_", "xoxb-", "xoxp-", "sk-"];

pub(crate) fn redact_args(args: &[String]) -> Vec<String> {
    let mut redact_next = false;
    args.iter()
        .map(|arg| {
            if redact_next {
                redact_next = false;
                return REDACTED.to_owned();
            }
            if is_sensitive_cli_flag(arg) {
                redact_next = true;
                return arg.clone();
            }
            redact_text(arg)
        })
        .collect()
}

pub(crate) fn redact_text(input: &str) -> String {
    let mut output = redact_url_userinfo(input);
    output = redact_bearer_tokens(&output);
    for name in ASSIGNMENT_NAMES {
        output = redact_assignment(&output, name);
    }
    for flag in CLI_FLAGS {
        output = redact_cli_flag(&output, flag);
    }
    for prefix in TOKEN_PREFIXES {
        output = redact_prefixed_token(&output, prefix);
    }
    output
}

fn is_sensitive_cli_flag(value: &str) -> bool {
    let normalized = value.trim().to_ascii_lowercase();
    CLI_FLAGS.iter().any(|flag| normalized == *flag)
}

fn redact_assignment(input: &str, name: &str) -> String {
    let mut output = input.to_owned();
    let mut search_start = 0;
    loop {
        let lower = output.to_ascii_lowercase();
        let Some(relative) = lower[search_start..].find(name) else {
            break;
        };
        let start = search_start + relative;
        if !has_name_boundary(&lower, start, name.len()) {
            search_start = start + name.len();
            continue;
        }
        let mut cursor = start + name.len();
        if lower
            .as_bytes()
            .get(cursor)
            .is_some_and(|byte| matches!(*byte, b'\'' | b'"'))
        {
            cursor += 1;
        }
        cursor = skip_ascii_whitespace(&lower, cursor);
        if !lower
            .as_bytes()
            .get(cursor)
            .is_some_and(|byte| matches!(*byte, b'=' | b':'))
        {
            search_start = start + name.len();
            continue;
        }
        cursor += 1;
        cursor = skip_ascii_whitespace(&lower, cursor);
        let quote = lower
            .as_bytes()
            .get(cursor)
            .copied()
            .filter(|byte| matches!(*byte, b'\'' | b'"'));
        if quote.is_some() {
            cursor += 1;
        }
        if cursor >= output.len() {
            break;
        }
        let end = value_end(&output, cursor, quote);
        if end == cursor {
            search_start = cursor.saturating_add(1);
            continue;
        }
        output.replace_range(cursor..end, REDACTED);
        search_start = cursor + REDACTED.len();
    }
    output
}

fn redact_cli_flag(input: &str, flag: &str) -> String {
    let mut output = input.to_owned();
    let mut search_start = 0;
    loop {
        let lower = output.to_ascii_lowercase();
        let Some(relative) = lower[search_start..].find(flag) else {
            break;
        };
        let start = search_start + relative;
        if !has_name_boundary(&lower, start, flag.len()) {
            search_start = start + flag.len();
            continue;
        }
        let mut cursor = start + flag.len();
        if lower.as_bytes().get(cursor) == Some(&b'=') {
            cursor += 1;
        } else {
            let skipped = skip_ascii_whitespace(&lower, cursor);
            if skipped == cursor {
                search_start = start + flag.len();
                continue;
            }
            cursor = skipped;
        }
        let quote = lower
            .as_bytes()
            .get(cursor)
            .copied()
            .filter(|byte| matches!(*byte, b'\'' | b'"'));
        if quote.is_some() {
            cursor += 1;
        }
        if cursor >= output.len() {
            break;
        }
        let end = value_end(&output, cursor, quote);
        if end == cursor {
            search_start = cursor.saturating_add(1);
            continue;
        }
        output.replace_range(cursor..end, REDACTED);
        search_start = cursor + REDACTED.len();
    }
    output
}

fn redact_bearer_tokens(input: &str) -> String {
    let mut output = input.to_owned();
    let mut search_start = 0;
    loop {
        let lower = output.to_ascii_lowercase();
        let Some(relative) = lower[search_start..].find("bearer") else {
            break;
        };
        let start = search_start + relative;
        if start > 0 && lower.as_bytes()[start - 1].is_ascii_alphanumeric() {
            search_start = start + "bearer".len();
            continue;
        }
        let token_start = skip_ascii_whitespace(&lower, start + "bearer".len());
        if token_start == start + "bearer".len() || token_start >= output.len() {
            search_start = start + "bearer".len();
            continue;
        }
        let token_end = value_end(&output, token_start, None);
        output.replace_range(token_start..token_end, REDACTED);
        search_start = token_start + REDACTED.len();
    }
    output
}

fn redact_prefixed_token(input: &str, prefix: &str) -> String {
    let mut output = input.to_owned();
    let mut search_start = 0;
    loop {
        let lower = output.to_ascii_lowercase();
        let Some(relative) = lower[search_start..].find(prefix) else {
            break;
        };
        let start = search_start + relative;
        if start > 0 && is_token_byte(lower.as_bytes()[start - 1]) {
            search_start = start + prefix.len();
            continue;
        }
        let end = value_end(&output, start, None);
        output.replace_range(start..end, REDACTED);
        search_start = start + REDACTED.len();
    }
    output
}

fn redact_url_userinfo(input: &str) -> String {
    let mut output = input.to_owned();
    let mut search_start = 0;
    loop {
        let Some(relative) = output[search_start..].find("://") else {
            break;
        };
        let authority_start = search_start + relative + 3;
        let authority_end = output[authority_start..]
            .char_indices()
            .find_map(|(offset, character)| {
                matches!(character, '/' | '?' | '#' | '\n' | '\r' | ' ' | '\t')
                    .then_some(authority_start + offset)
            })
            .unwrap_or(output.len());
        let authority = &output[authority_start..authority_end];
        let Some(at_offset) = authority.rfind('@') else {
            search_start = authority_end;
            continue;
        };
        let userinfo = &authority[..at_offset];
        let Some(colon_offset) = userinfo.find(':') else {
            search_start = authority_end;
            continue;
        };
        let secret_start = authority_start + colon_offset + 1;
        let secret_end = authority_start + at_offset;
        if secret_start < secret_end {
            output.replace_range(secret_start..secret_end, REDACTED);
            search_start = secret_start + REDACTED.len();
        } else {
            search_start = authority_end;
        }
    }
    output
}

fn has_name_boundary(value: &str, start: usize, len: usize) -> bool {
    let bytes = value.as_bytes();
    let before_ok = start == 0 || !is_name_byte(bytes[start - 1]);
    let end = start + len;
    let after_ok = end >= bytes.len() || !is_name_byte(bytes[end]);
    before_ok && after_ok
}

fn is_name_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-')
}

fn is_token_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-')
}

fn skip_ascii_whitespace(value: &str, mut cursor: usize) -> usize {
    while value
        .as_bytes()
        .get(cursor)
        .is_some_and(u8::is_ascii_whitespace)
    {
        cursor += 1;
    }
    cursor
}

fn value_end(value: &str, start: usize, quote: Option<u8>) -> usize {
    for (offset, character) in value[start..].char_indices() {
        let byte = character as u32;
        if quote.is_some_and(|quote| byte == u32::from(quote))
            || (quote.is_none()
                && (character.is_whitespace()
                    || matches!(character, '&' | ';' | ',' | '\'' | '"' | ']' | '}')))
        {
            return start + offset;
        }
    }
    value.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_assignments_headers_and_common_token_prefixes() {
        let input =
            "api_key=alpha Authorization: Bearer bearer-secret password=bravo sk-charlie ghp_delta";
        let redacted = redact_text(input);
        for secret in ["alpha", "bearer-secret", "bravo", "sk-charlie", "ghp_delta"] {
            assert!(!redacted.contains(secret));
        }
        assert!(redacted.contains(REDACTED));
    }

    #[test]
    fn redacts_signed_urls_and_connection_string_credentials() {
        let input = "postgres://user:db-password@example.test/db?X-Amz-Signature=signed-secret&token=query-secret";
        let redacted = redact_text(input);
        for secret in ["db-password", "signed-secret", "query-secret"] {
            assert!(!redacted.contains(secret));
        }
        assert!(redacted.contains("postgres://user:[REDACTED]@example.test"));
    }

    #[test]
    fn redacts_separate_cli_secret_arguments() {
        let args = vec![
            "--password".to_owned(),
            "cli-secret".to_owned(),
            "--api-key=inline-secret".to_owned(),
            "https://example.test/?sig=url-secret".to_owned(),
        ];
        let redacted = redact_args(&args);
        let joined = redacted.join(" ");
        for secret in ["cli-secret", "inline-secret", "url-secret"] {
            assert!(!joined.contains(secret));
        }
    }

    #[test]
    fn preserves_non_secret_command_content() {
        let input = "cargo test -p medusa-agent output_redaction";
        assert_eq!(redact_text(input), input);
    }
}
