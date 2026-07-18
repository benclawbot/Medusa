use super::support::StyledLine;
use super::*;

pub(super) fn markdown_block_lines(
    first_marker: &str,
    marker_color: Color,
    markdown: &str,
    width: u16,
) -> Vec<StyledLine> {
    let marker_width = first_marker.chars().count();
    let content_width = usize::from(width).saturating_sub(marker_width).max(1);
    let continuation = " ".repeat(marker_width);
    let mut rendered = Vec::new();
    let mut first = true;
    let mut in_code = false;

    for source in markdown.split('\n') {
        let trimmed = source.trim_start();
        if let Some(language) = trimmed.strip_prefix("```") {
            if in_code {
                push_wrapped(
                    &mut rendered,
                    &mut first,
                    first_marker,
                    &continuation,
                    marker_color,
                    "└─",
                    Color::DarkGrey,
                    Some(Color::DarkGrey),
                    Attribute::Reset,
                    true,
                    content_width,
                );
            } else {
                let label = language.trim();
                let header = if label.is_empty() {
                    "┌─ code".to_owned()
                } else {
                    format!("┌─ {label}")
                };
                push_wrapped(
                    &mut rendered,
                    &mut first,
                    first_marker,
                    &continuation,
                    marker_color,
                    &header,
                    Color::Cyan,
                    Some(Color::DarkGrey),
                    Attribute::Bold,
                    true,
                    content_width,
                );
            }
            in_code = !in_code;
            continue;
        }

        if in_code {
            push_code_line(
                &mut rendered,
                &mut first,
                first_marker,
                &continuation,
                marker_color,
                source,
                content_width,
            );
            continue;
        }

        if trimmed.is_empty() {
            push_wrapped(
                &mut rendered,
                &mut first,
                first_marker,
                &continuation,
                marker_color,
                "",
                Color::White,
                None,
                Attribute::Reset,
                false,
                content_width,
            );
            continue;
        }

        let (text, foreground, attribute) = markdown_line(trimmed, content_width);
        push_wrapped(
            &mut rendered,
            &mut first,
            first_marker,
            &continuation,
            marker_color,
            &text,
            foreground,
            None,
            attribute,
            false,
            content_width,
        );
    }

    if rendered.is_empty() {
        rendered.push(StyledLine::with_marker(
            first_marker,
            marker_color,
            "",
            Color::White,
        ));
    }
    rendered
}

fn markdown_line(source: &str, width: usize) -> (String, Color, Attribute) {
    if let Some((level, heading)) = heading(source) {
        let marker = match level {
            1 => "◆ ",
            2 => "◇ ",
            _ => "· ",
        };
        return (
            format!("{marker}{}", clean_inline(heading)),
            Color::Cyan,
            Attribute::Bold,
        );
    }
    if is_rule(source) {
        return ("─".repeat(width.min(48)), Color::DarkGrey, Attribute::Reset);
    }
    if let Some(quote) = source.strip_prefix('>') {
        return (
            format!("│ {}", clean_inline(quote.trim_start())),
            Color::Grey,
            Attribute::Italic,
        );
    }
    for (prefix, glyph) in [("- [x] ", "✓ "), ("- [X] ", "✓ "), ("- [ ] ", "□ ")] {
        if let Some(item) = source.strip_prefix(prefix) {
            return (
                format!("{glyph}{}", clean_inline(item)),
                if glyph.starts_with('✓') {
                    Color::Green
                } else {
                    Color::Grey
                },
                Attribute::Reset,
            );
        }
    }
    for prefix in ["- ", "* ", "+ "] {
        if let Some(item) = source.strip_prefix(prefix) {
            return (
                format!("• {}", clean_inline(item)),
                Color::White,
                Attribute::Reset,
            );
        }
    }
    if ordered_item(source) {
        return (clean_inline(source), Color::White, Attribute::Reset);
    }
    (clean_inline(source), Color::White, Attribute::Reset)
}

fn heading(source: &str) -> Option<(usize, &str)> {
    let level = source
        .chars()
        .take_while(|character| *character == '#')
        .count();
    (1..=6)
        .contains(&level)
        .then(|| source.get(level..))
        .flatten()
        .and_then(|rest| rest.strip_prefix(' '))
        .map(|heading| (level, heading))
}

fn is_rule(source: &str) -> bool {
    let compact = source
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect::<String>();
    compact.len() >= 3
        && compact
            .chars()
            .all(|character| character == '-' || character == '*' || character == '_')
}

fn ordered_item(source: &str) -> bool {
    let digits = source.chars().take_while(char::is_ascii_digit).count();
    digits > 0
        && source
            .get(digits..)
            .is_some_and(|rest| rest.starts_with(". "))
}

fn clean_inline(source: &str) -> String {
    let mut value = source
        .replace("**", "")
        .replace("__", "")
        .replace("~~", "")
        .replace('`', "");
    while let Some(start) = value.find('[') {
        let Some(label_end) = value[start + 1..]
            .find("](")
            .map(|offset| start + 1 + offset)
        else {
            break;
        };
        let url_start = label_end + 2;
        let Some(url_end) = value[url_start..]
            .find(')')
            .map(|offset| url_start + offset)
        else {
            break;
        };
        let label = value[start + 1..label_end].to_owned();
        let url = value[url_start..url_end].to_owned();
        value.replace_range(start..=url_end, &format!("{label} ({url})"));
    }
    value
}

#[allow(clippy::too_many_arguments)]
fn push_wrapped(
    rendered: &mut Vec<StyledLine>,
    first: &mut bool,
    first_marker: &str,
    continuation: &str,
    marker_color: Color,
    text: &str,
    foreground: Color,
    background: Option<Color>,
    attribute: Attribute,
    fill_background: bool,
    width: usize,
) {
    let rows = wrap_words(text, width);
    push_rows(
        rendered,
        first,
        first_marker,
        continuation,
        marker_color,
        rows,
        foreground,
        background,
        attribute,
        fill_background,
    );
}

fn push_code_line(
    rendered: &mut Vec<StyledLine>,
    first: &mut bool,
    first_marker: &str,
    continuation: &str,
    marker_color: Color,
    source: &str,
    width: usize,
) {
    let code_width = width.saturating_sub(2).max(1);
    let rows = if source.is_empty() {
        vec!["│ ".to_owned()]
    } else {
        source
            .chars()
            .collect::<Vec<_>>()
            .chunks(code_width)
            .map(|chunk| format!("│ {}", chunk.iter().collect::<String>()))
            .collect()
    };
    push_rows(
        rendered,
        first,
        first_marker,
        continuation,
        marker_color,
        rows,
        Color::White,
        Some(Color::DarkGrey),
        Attribute::Reset,
        true,
    );
}

#[allow(clippy::too_many_arguments)]
fn push_rows(
    rendered: &mut Vec<StyledLine>,
    first: &mut bool,
    first_marker: &str,
    continuation: &str,
    marker_color: Color,
    rows: Vec<String>,
    foreground: Color,
    background: Option<Color>,
    attribute: Attribute,
    fill_background: bool,
) {
    for row in rows {
        rendered.push(StyledLine::with_marker_style(
            if *first { first_marker } else { continuation },
            marker_color,
            row,
            foreground,
            background,
            attribute,
            fill_background,
        ));
        *first = false;
    }
}

fn wrap_words(text: &str, width: usize) -> Vec<String> {
    if text.is_empty() {
        return vec![String::new()];
    }
    let mut rows = Vec::new();
    let mut current = String::new();
    for word in text.split_whitespace() {
        if word.chars().count() > width {
            if !current.is_empty() {
                rows.push(std::mem::take(&mut current));
            }
            let characters = word.chars().collect::<Vec<_>>();
            rows.extend(
                characters
                    .chunks(width)
                    .map(|chunk| chunk.iter().collect::<String>()),
            );
            continue;
        }
        let required =
            current.chars().count() + usize::from(!current.is_empty()) + word.chars().count();
        if required > width && !current.is_empty() {
            rows.push(std::mem::take(&mut current));
        }
        if !current.is_empty() {
            current.push(' ');
        }
        current.push_str(word);
    }
    if !current.is_empty() {
        rows.push(current);
    }
    if rows.is_empty() {
        rows.push(String::new());
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markdown_renders_headings_lists_quotes_links_and_code() {
        let lines = markdown_block_lines(
            "Medusa  ",
            Color::Magenta,
            "# Result\n\n- [x] fixed\n> note\n[docs](https://example.test)\n```rust\nfn main() {}\n```",
            80,
        );
        let rendered = lines
            .iter()
            .map(|line| line.text.as_str())
            .collect::<Vec<_>>();
        assert!(rendered.iter().any(|line| line.contains("◆ Result")));
        assert!(rendered.iter().any(|line| line.contains("✓ fixed")));
        assert!(rendered.iter().any(|line| line.contains("│ note")));
        assert!(
            rendered
                .iter()
                .any(|line| line.contains("docs (https://example.test)"))
        );
        assert!(rendered.iter().any(|line| line.contains("┌─ rust")));
        assert!(rendered.iter().any(|line| line.contains("│ fn main() {}")));
        assert!(rendered.iter().any(|line| line == &"└─"));
    }

    #[test]
    fn fenced_code_preserves_indentation_and_repeated_spaces() {
        let lines = markdown_block_lines(
            "Medusa  ",
            Color::Magenta,
            "```python\nif ready:\n    print(\"a  b\")\n```",
            80,
        );
        let rendered = lines
            .iter()
            .map(|line| line.text.as_str())
            .collect::<Vec<_>>();
        assert!(rendered.iter().any(|line| *line == "│     print(\"a  b\")"));
    }

    #[test]
    fn markdown_wraps_without_repeating_the_speaker_marker() {
        let lines = markdown_block_lines("Medusa  ", Color::Magenta, "one two three four", 16);
        assert!(lines.len() > 1);
        assert_eq!(
            lines[0].marker.as_ref().map(|marker| marker.0.as_str()),
            Some("Medusa  ")
        );
        assert!(lines.iter().skip(1).all(|line| {
            line.marker
                .as_ref()
                .is_some_and(|marker| marker.0.trim().is_empty())
        }));
    }
}
