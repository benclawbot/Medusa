const CODE_FENCE: &str = "```\n";

#[must_use]
pub fn utf16_len(value: &str) -> usize {
    value.encode_utf16().count()
}

#[must_use]
pub fn split_telegram_text(value: &str, limit_utf16: usize) -> Vec<String> {
    if value.is_empty() {
        return Vec::new();
    }
    if limit_utf16 == 0 || utf16_len(value) <= limit_utf16 {
        return vec![value.to_owned()];
    }

    let mut chunks = Vec::new();
    let mut current = String::new();
    let mut current_len = 0usize;
    let mut fenced = false;

    for line in value.split_inclusive('\n') {
        let line_len = utf16_len(line);
        if current_len + line_len > limit_utf16 && !current.is_empty() {
            close_fence_if_possible(&mut current, &mut current_len, fenced, limit_utf16);
            chunks.push(current);
            current = if fenced {
                CODE_FENCE.to_owned()
            } else {
                String::new()
            };
            current_len = utf16_len(&current);
        }

        if line_len > limit_utf16 {
            for character in line.chars() {
                let character_len = character.len_utf16();
                if current_len + character_len > limit_utf16 && !current.is_empty() {
                    close_fence_if_possible(&mut current, &mut current_len, fenced, limit_utf16);
                    chunks.push(current);
                    current = if fenced {
                        CODE_FENCE.to_owned()
                    } else {
                        String::new()
                    };
                    current_len = utf16_len(&current);
                }
                current.push(character);
                current_len += character_len;
            }
        } else {
            current.push_str(line);
            current_len += line_len;
        }

        if line.trim_start().starts_with("```") {
            fenced = !fenced;
        }
    }

    if !current.is_empty() {
        close_fence_if_possible(&mut current, &mut current_len, fenced, limit_utf16);
        chunks.push(current);
    }
    chunks
}

fn close_fence_if_possible(
    current: &mut String,
    current_len: &mut usize,
    fenced: bool,
    limit_utf16: usize,
) {
    let fence_len = utf16_len(CODE_FENCE);
    if fenced && *current_len + fence_len <= limit_utf16 {
        current.push_str(CODE_FENCE);
        *current_len += fence_len;
    }
}

#[must_use]
pub fn telegram_markdown_v2(value: &str) -> String {
    let normalized = normalize_markdown_tables(value);
    let mut escaped = String::with_capacity(normalized.len());
    let mut fenced = false;
    let mut inline_code = false;
    let mut characters = normalized.chars().peekable();

    while let Some(character) = characters.next() {
        if character == '`' {
            if characters.peek() == Some(&'`') {
                let mut lookahead = characters.clone();
                lookahead.next();
                if lookahead.peek() == Some(&'`') {
                    characters.next();
                    characters.next();
                    escaped.push_str("```");
                    fenced = !fenced;
                    continue;
                }
            }
            if !fenced {
                inline_code = !inline_code;
            }
            escaped.push('`');
            continue;
        }

        if fenced || inline_code {
            if matches!(character, '\\' | '`') {
                escaped.push('\\');
            }
            escaped.push(character);
            continue;
        }

        if is_markdown_v2_special(character) {
            escaped.push('\\');
        }
        escaped.push(character);
    }
    escaped
}

fn is_markdown_v2_special(character: char) -> bool {
    matches!(
        character,
        '_' | '*'
            | '['
            | ']'
            | '('
            | ')'
            | '~'
            | '>'
            | '#'
            | '+'
            | '-'
            | '='
            | '|'
            | '{'
            | '}'
            | '.'
            | '!'
            | '\\'
    )
}

#[must_use]
pub fn normalize_markdown_tables(value: &str) -> String {
    let lines = value.lines().collect::<Vec<_>>();
    let mut output = Vec::new();
    let mut index = 0usize;

    while index < lines.len() {
        if index + 1 < lines.len()
            && lines[index].contains('|')
            && is_table_separator(lines[index + 1])
        {
            let headers = table_cells(lines[index]);
            index += 2;
            while index < lines.len() && lines[index].contains('|') {
                let cells = table_cells(lines[index]);
                let row = headers
                    .iter()
                    .zip(cells.iter())
                    .map(|(header, cell)| format!("{header}: {cell}"))
                    .collect::<Vec<_>>()
                    .join("; ");
                output.push(format!("• {row}"));
                index += 1;
            }
            continue;
        }
        output.push(lines[index].to_owned());
        index += 1;
    }
    output.join("\n")
}

fn table_cells(line: &str) -> Vec<String> {
    line.trim_matches('|')
        .split('|')
        .map(|cell| cell.trim().to_owned())
        .filter(|cell| !cell.is_empty())
        .collect()
}

fn is_table_separator(line: &str) -> bool {
    let cells = table_cells(line);
    !cells.is_empty()
        && cells.iter().all(|cell| {
            let trimmed = cell.trim_matches(':').trim();
            trimmed.len() >= 3 && trimmed.bytes().all(|byte| byte == b'-')
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf16_splitting_preserves_non_bmp_characters() {
        assert_eq!(utf16_len("a😀b"), 4);
        let chunks = split_telegram_text("😀😀😀😀", 4);
        assert_eq!(chunks, vec!["😀😀", "😀😀"]);
        assert!(chunks.iter().all(|chunk| utf16_len(chunk) <= 4));
    }

    #[test]
    fn markdown_specials_and_tables_are_normalized() {
        let rendered = telegram_markdown_v2(
            "| Name | State |\n| --- | --- |\n| Worker | active |\nhello_world!",
        );
        assert!(rendered.contains("• Name: Worker; State: active"));
        assert!(rendered.contains("hello\\_world\\!"));
    }
}
