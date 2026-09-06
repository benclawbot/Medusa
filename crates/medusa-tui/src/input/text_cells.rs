const TAB_STOP: usize = 8;

pub(crate) fn previous_grapheme_boundary(text: &str, cursor: usize) -> usize {
    let target = cursor.min(text.len());
    if target == 0 {
        return 0;
    }
    let mut boundary = 0;
    let mut next = 0;
    while next < target {
        boundary = next;
        next = next_grapheme_boundary(text, next);
        if next >= target {
            return boundary;
        }
    }
    boundary
}

pub(crate) fn next_grapheme_boundary(text: &str, cursor: usize) -> usize {
    let start = cursor.min(text.len());
    if start >= text.len() {
        return text.len();
    }
    let mut chars = text[start..].char_indices();
    let Some((_, first)) = chars.next() else {
        return text.len();
    };
    let mut end = start + first.len_utf8();

    if first == '\r' && text[end..].starts_with('\n') {
        return end + 1;
    }

    if is_regional_indicator(first) {
        if let Some(next) = text[end..].chars().next()
            && is_regional_indicator(next)
        {
            end += next.len_utf8();
        }
        return consume_grapheme_extenders(text, end);
    }

    end = consume_grapheme_extenders(text, end);
    loop {
        let Some(next) = text[end..].chars().next() else {
            break;
        };
        if next != '\u{200d}' {
            break;
        }
        end += next.len_utf8();
        let Some(joined) = text[end..].chars().next() else {
            break;
        };
        end += joined.len_utf8();
        end = consume_grapheme_extenders(text, end);
    }
    end
}

pub(crate) fn cell_column(text: &str, cursor: usize) -> usize {
    let target = cursor.min(text.len());
    let mut byte = 0;
    let mut column = 0;
    while byte < target {
        let next = next_grapheme_boundary(text, byte).min(target);
        column += grapheme_cell_width(&text[byte..next], column);
        byte = next;
    }
    column
}

pub(crate) fn byte_at_cell_column(text: &str, target_column: usize) -> usize {
    let mut byte = 0;
    let mut column = 0;
    while byte < text.len() {
        let next = next_grapheme_boundary(text, byte);
        let width = grapheme_cell_width(&text[byte..next], column);
        if column.saturating_add(width) > target_column {
            break;
        }
        column = column.saturating_add(width);
        byte = next;
        if column == target_column {
            break;
        }
    }
    byte
}

pub(crate) fn byte_range_for_cell_range(
    text: &str,
    start_column: usize,
    end_column: usize,
) -> Option<(usize, usize)> {
    if start_column >= end_column {
        return None;
    }
    let mut byte = 0;
    let mut column = 0;
    let mut first_byte = None;
    let mut last_byte = None;
    while byte < text.len() {
        let next = next_grapheme_boundary(text, byte);
        let cells = grapheme_cell_width(&text[byte..next], column);
        let cell_end = column.saturating_add(cells);
        if cell_end > start_column && column < end_column {
            first_byte.get_or_insert(byte);
            last_byte = Some(next);
        }
        if column >= end_column {
            break;
        }
        column = cell_end;
        byte = next;
    }
    first_byte.zip(last_byte)
}

pub(crate) fn display_width(text: &str) -> usize {
    let mut widest = 0;
    let mut column: usize = 0;
    let mut byte = 0;
    while byte < text.len() {
        let next = next_grapheme_boundary(text, byte);
        let grapheme = &text[byte..next];
        if grapheme == "\n" || grapheme == "\r\n" {
            widest = widest.max(column);
            column = 0;
        } else {
            column = column.saturating_add(grapheme_cell_width(grapheme, column));
        }
        byte = next;
    }
    widest.max(column)
}

pub(crate) fn fit_to_cells(value: &str, width: usize) -> String {
    let line = value.split('\n').next().unwrap_or_default();
    if width == 0 {
        return String::new();
    }
    let mut output = String::new();
    let mut column = 0;
    let mut byte = 0;
    while byte < line.len() {
        let next = next_grapheme_boundary(line, byte);
        let grapheme = &line[byte..next];
        let cells = grapheme_cell_width(grapheme, column);
        if column.saturating_add(cells) > width {
            break;
        }
        output.push_str(grapheme);
        column = column.saturating_add(cells);
        byte = next;
    }
    output
}

pub(crate) fn wrap_to_cells(value: &str, width: usize) -> String {
    if width == 0 {
        return value.to_owned();
    }
    let mut output = String::with_capacity(value.len());
    let mut column = 0;
    let mut byte = 0;
    while byte < value.len() {
        let next = next_grapheme_boundary(value, byte);
        let grapheme = &value[byte..next];
        if grapheme == "\n" || grapheme == "\r\n" {
            output.push('\n');
            column = 0;
            byte = next;
            continue;
        }
        let mut cells = grapheme_cell_width(grapheme, column);
        if column > 0 && column.saturating_add(cells) > width {
            output.push('\n');
            column = 0;
            cells = grapheme_cell_width(grapheme, column);
        }
        if cells <= width || column == 0 {
            output.push_str(grapheme);
            column = column.saturating_add(cells);
        }
        byte = next;
    }
    output
}

pub(crate) fn center_or_crop_cells(line: &str, block_width: usize, width: usize) -> String {
    if width >= block_width {
        return format!("{}{}", " ".repeat((width - block_width) / 2), line);
    }
    let start = (block_width - width) / 2;
    let mut output = String::new();
    let mut column = 0;
    let mut byte = 0;
    while byte < line.len() {
        let next = next_grapheme_boundary(line, byte);
        let grapheme = &line[byte..next];
        let cells = grapheme_cell_width(grapheme, column);
        let end = column.saturating_add(cells);
        if end > start && column < start.saturating_add(width) {
            if display_width(&output).saturating_add(cells) > width {
                break;
            }
            output.push_str(grapheme);
        }
        if column >= start.saturating_add(width) {
            break;
        }
        column = end;
        byte = next;
    }
    output
}

fn grapheme_cell_width(grapheme: &str, column: usize) -> usize {
    if grapheme == "\t" {
        return TAB_STOP - (column % TAB_STOP);
    }
    if grapheme.contains('\u{200d}') || grapheme.contains('\u{fe0f}') {
        return 2;
    }
    let mut width = 0;
    for character in grapheme.chars() {
        width = width.max(character_cell_width(character));
    }
    width
}

fn character_cell_width(character: char) -> usize {
    let code = character as u32;
    if character == '\0'
        || character == '\u{200d}'
        || character.is_control()
        || is_grapheme_extender(character)
    {
        return 0;
    }
    if is_wide(code) { 2 } else { 1 }
}

fn is_wide(code: u32) -> bool {
    matches!(
        code,
        0x1100..=0x115f
            | 0x231a..=0x231b
            | 0x2329..=0x232a
            | 0x23e9..=0x23ec
            | 0x23f0
            | 0x23f3
            | 0x25fd..=0x25fe
            | 0x2614..=0x2615
            | 0x2648..=0x2653
            | 0x267f
            | 0x2693
            | 0x26a1
            | 0x26aa..=0x26ab
            | 0x26bd..=0x26be
            | 0x26c4..=0x26c5
            | 0x26ce
            | 0x26d4
            | 0x26ea
            | 0x26f2..=0x26f3
            | 0x26f5
            | 0x26fa
            | 0x26fd
            | 0x2705
            | 0x270a..=0x270b
            | 0x2728
            | 0x274c
            | 0x274e
            | 0x2753..=0x2755
            | 0x2757
            | 0x2795..=0x2797
            | 0x27b0
            | 0x27bf
            | 0x2b1b..=0x2b1c
            | 0x2b50
            | 0x2b55
            | 0x2e80..=0x303e
            | 0x3040..=0xa4cf
            | 0xac00..=0xd7a3
            | 0xf900..=0xfaff
            | 0xfe10..=0xfe19
            | 0xfe30..=0xfe6f
            | 0xff01..=0xff60
            | 0xffe0..=0xffe6
            | 0x1f004
            | 0x1f0cf
            | 0x1f18e
            | 0x1f191..=0x1f19a
            | 0x1f1e6..=0x1f1ff
            | 0x1f201..=0x1f202
            | 0x1f21a
            | 0x1f22f
            | 0x1f232..=0x1f23a
            | 0x1f250..=0x1f251
            | 0x1f300..=0x1faff
            | 0x20000..=0x3fffd
    )
}

fn consume_grapheme_extenders(text: &str, mut cursor: usize) -> usize {
    while let Some(character) = text[cursor..].chars().next() {
        if !is_grapheme_extender(character) {
            break;
        }
        cursor += character.len_utf8();
    }
    cursor
}

fn is_grapheme_extender(character: char) -> bool {
    matches!(
        character as u32,
        0x0300..=0x036f
            | 0x0483..=0x0489
            | 0x0591..=0x05bd
            | 0x05bf
            | 0x05c1..=0x05c2
            | 0x05c4..=0x05c5
            | 0x0610..=0x061a
            | 0x064b..=0x065f
            | 0x0670
            | 0x06d6..=0x06dc
            | 0x06df..=0x06e4
            | 0x06e7..=0x06e8
            | 0x06ea..=0x06ed
            | 0x0711
            | 0x0730..=0x074a
            | 0x07a6..=0x07b0
            | 0x07eb..=0x07f3
            | 0x0816..=0x0819
            | 0x081b..=0x0823
            | 0x0825..=0x0827
            | 0x0829..=0x082d
            | 0x0859..=0x085b
            | 0x08d3..=0x0902
            | 0x093a
            | 0x093c
            | 0x0941..=0x0948
            | 0x094d
            | 0x0951..=0x0957
            | 0x0962..=0x0963
            | 0x1ab0..=0x1aff
            | 0x1dc0..=0x1dff
            | 0x20d0..=0x20ff
            | 0xfe00..=0xfe0f
            | 0xfe20..=0xfe2f
            | 0x1f3fb..=0x1f3ff
            | 0xe0020..=0xe007f
            | 0xe0100..=0xe01ef
    )
}

fn is_regional_indicator(character: char) -> bool {
    matches!(character as u32, 0x1f1e6..=0x1f1ff)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn measures_cjk_combining_emoji_flags_and_tabs_in_terminal_cells() {
        assert_eq!(display_width("界"), 2);
        assert_eq!(display_width("e\u{301}"), 1);
        assert_eq!(display_width("👩‍💻"), 2);
        assert_eq!(display_width("🇨🇭"), 2);
        assert_eq!(display_width("a\tb"), 9);
    }

    #[test]
    fn wrapping_never_splits_supported_grapheme_clusters() {
        assert_eq!(wrap_to_cells("界界", 2), "界\n界");
        assert_eq!(wrap_to_cells("e\u{301}e\u{301}", 1), "e\u{301}\ne\u{301}");
        assert_eq!(wrap_to_cells("👩‍💻👩‍💻", 2), "👩‍💻\n👩‍💻");
        for row in wrap_to_cells("界e\u{301}👩‍💻", 3).lines() {
            assert!(display_width(row) <= 3);
        }
    }

    #[test]
    fn cell_column_mapping_respects_wide_clusters() {
        let text = "a界b";
        let after_a = "a".len();
        let after_cjk = "a界".len();
        assert_eq!(cell_column(text, after_a), 1);
        assert_eq!(cell_column(text, after_cjk), 3);
        assert_eq!(byte_at_cell_column(text, 1), after_a);
        assert_eq!(byte_at_cell_column(text, 2), after_a);
        assert_eq!(byte_at_cell_column(text, 3), after_cjk);
    }

    #[test]
    fn selection_cell_ranges_expand_to_whole_graphemes() {
        let text = "a界e\u{301}👩‍💻b";
        let cjk = byte_range_for_cell_range(text, 2, 3).expect("second CJK cell");
        assert_eq!(&text[cjk.0..cjk.1], "界");

        let combining_start = cell_column(text, "a界".len());
        let combining = byte_range_for_cell_range(text, combining_start, combining_start + 1)
            .expect("combining grapheme");
        assert_eq!(&text[combining.0..combining.1], "e\u{301}");

        let emoji_start = cell_column(text, "a界e\u{301}".len());
        let emoji = byte_range_for_cell_range(text, emoji_start + 1, emoji_start + 2)
            .expect("second emoji cell");
        assert_eq!(&text[emoji.0..emoji.1], "👩‍💻");
    }

    #[test]
    fn fit_and_center_crop_use_cells_without_splitting_clusters() {
        assert_eq!(fit_to_cells("界abc", 3), "界a");
        assert_eq!(fit_to_cells("e\u{301}x", 1), "e\u{301}");
        let cropped = center_or_crop_cells("a界bc", display_width("a界bc"), 3);
        assert!(display_width(&cropped) <= 3);
        assert!(!cropped.starts_with('\u{301}'));
    }
}
