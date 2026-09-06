use std::cell::RefCell;

use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::clipboard::{ClipboardError, PromptDraft};

#[allow(dead_code)]
mod legacy;

pub use legacy::{MenuItem, SelectionState, select_menu, select_menu_items};

const MAX_PROMPT_HISTORY: usize = 100;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ComposerAction {
    None,
    Changed,
    Submit,
    Interrupt,
    CommandPrevious,
    CommandNext,
    CompleteCommand,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ComposerState {
    pub draft: PromptDraft,
    /// UTF-8 byte offset. Every edit/navigation path keeps this on a grapheme boundary.
    pub cursor: usize,
}

#[derive(Default)]
struct PromptHistory {
    entries: Vec<PromptDraft>,
    position: Option<usize>,
    saved_draft: Option<PromptDraft>,
}

thread_local! {
    static PROMPT_HISTORY: RefCell<PromptHistory> = RefCell::new(PromptHistory::default());
}

impl ComposerState {
    #[must_use]
    pub fn new(initial_text: impl Into<String>) -> Self {
        let text = initial_text.into();
        let cursor = text.len();
        reset_history_navigation();
        Self {
            draft: PromptDraft {
                text,
                ..PromptDraft::default()
            },
            cursor,
        }
    }

    pub fn handle_event(&mut self, event: Event) -> Result<ComposerAction, ClipboardError> {
        match event {
            Event::Paste(text) => {
                self.draft.insert_pasted_text(self.cursor, &text)?;
                self.cursor += normalized_len(&text);
                reset_history_navigation();
                Ok(ComposerAction::Changed)
            }
            Event::Key(key) if key.kind != KeyEventKind::Release => self.handle_key(key),
            _ => Ok(ComposerAction::None),
        }
    }

    fn handle_key(&mut self, key: KeyEvent) -> Result<ComposerAction, ClipboardError> {
        match (key.code, key.modifiers) {
            (KeyCode::Enter, KeyModifiers::NONE) => {
                if self.draft.text.trim().is_empty() && self.draft.attachments.is_empty() {
                    Ok(ComposerAction::None)
                } else {
                    record_history(&self.draft);
                    Ok(ComposerAction::Submit)
                }
            }
            (KeyCode::Enter, modifiers) if modifiers.contains(KeyModifiers::SHIFT) => {
                self.insert_text("\n")
            }
            (KeyCode::Char('c'), modifiers) if modifiers.contains(KeyModifiers::CONTROL) => {
                Ok(ComposerAction::Interrupt)
            }
            (KeyCode::Up, _) if self.slash_completion_navigation_active() => {
                Ok(ComposerAction::CommandPrevious)
            }
            (KeyCode::Down, _) if self.slash_completion_navigation_active() => {
                Ok(ComposerAction::CommandNext)
            }
            (KeyCode::Up, _) => Ok(self.move_vertical(-1)),
            (KeyCode::Down, _) => Ok(self.move_vertical(1)),
            (KeyCode::Tab, _) => Ok(ComposerAction::CompleteCommand),
            (KeyCode::Home, modifiers) if modifiers.contains(KeyModifiers::CONTROL) => {
                self.cursor = 0;
                Ok(ComposerAction::None)
            }
            (KeyCode::End, modifiers) if modifiers.contains(KeyModifiers::CONTROL) => {
                self.cursor = self.draft.text.len();
                Ok(ComposerAction::None)
            }
            (KeyCode::Home, _) => {
                self.cursor = line_start(&self.draft.text, self.cursor);
                Ok(ComposerAction::None)
            }
            (KeyCode::End, _) => {
                self.cursor = line_end(&self.draft.text, self.cursor);
                Ok(ComposerAction::None)
            }
            (KeyCode::Delete, _) => self.delete_forward(),
            (KeyCode::Left, modifiers) if modifiers.contains(KeyModifiers::CONTROL) => {
                self.cursor = previous_word_boundary(&self.draft.text, self.cursor);
                Ok(ComposerAction::None)
            }
            (KeyCode::Right, modifiers) if modifiers.contains(KeyModifiers::CONTROL) => {
                self.cursor = next_word_boundary(&self.draft.text, self.cursor);
                Ok(ComposerAction::None)
            }
            (KeyCode::Char(character), modifiers)
                if modifiers.is_empty() || modifiers == KeyModifiers::SHIFT =>
            {
                let mut buffer = [0_u8; 4];
                self.insert_text(character.encode_utf8(&mut buffer))
            }
            (KeyCode::Backspace, _) => {
                if self.cursor == 0 {
                    return Ok(ComposerAction::None);
                }
                let previous = previous_grapheme_boundary(&self.draft.text, self.cursor);
                self.draft.text.replace_range(previous..self.cursor, "");
                self.cursor = previous;
                self.draft.revision = self.draft.revision.saturating_add(1);
                reset_history_navigation();
                Ok(ComposerAction::Changed)
            }
            (KeyCode::Left, _) => {
                self.cursor = previous_grapheme_boundary(&self.draft.text, self.cursor);
                Ok(ComposerAction::None)
            }
            (KeyCode::Right, _) => {
                self.cursor = next_grapheme_boundary(&self.draft.text, self.cursor);
                Ok(ComposerAction::None)
            }
            _ => Ok(ComposerAction::None),
        }
    }

    fn insert_text(&mut self, text: &str) -> Result<ComposerAction, ClipboardError> {
        self.draft.insert_pasted_text(self.cursor, text)?;
        self.cursor += text.len();
        reset_history_navigation();
        Ok(ComposerAction::Changed)
    }

    fn delete_forward(&mut self) -> Result<ComposerAction, ClipboardError> {
        if self.cursor >= self.draft.text.len() {
            return Ok(ComposerAction::None);
        }
        let next = next_grapheme_boundary(&self.draft.text, self.cursor);
        self.draft.text.replace_range(self.cursor..next, "");
        self.draft.revision = self.draft.revision.saturating_add(1);
        reset_history_navigation();
        Ok(ComposerAction::Changed)
    }

    fn slash_completion_navigation_active(&self) -> bool {
        !self.draft.text.contains('\n') && self.draft.text.starts_with('/')
    }

    fn move_vertical(&mut self, direction: isize) -> ComposerAction {
        let text = &self.draft.text;
        let line_start = line_start(text, self.cursor);
        let line_end = line_end(text, self.cursor);
        let column = grapheme_count(&text[line_start..self.cursor]);

        if direction < 0 {
            if line_start == 0 {
                return if self.history_previous() {
                    ComposerAction::Changed
                } else {
                    ComposerAction::None
                };
            }
            let previous_end = line_start - 1;
            let previous_start = text[..previous_end]
                .rfind('\n')
                .map_or(0, |index| index + 1);
            let offset = grapheme_boundary_at_column(&text[previous_start..previous_end], column);
            self.cursor = previous_start + offset;
            ComposerAction::None
        } else {
            if line_end == text.len() {
                return if self.history_next() {
                    ComposerAction::Changed
                } else {
                    ComposerAction::None
                };
            }
            let next_start = line_end + 1;
            let next_end = text[next_start..]
                .find('\n')
                .map_or(text.len(), |offset| next_start + offset);
            let offset = grapheme_boundary_at_column(&text[next_start..next_end], column);
            self.cursor = next_start + offset;
            ComposerAction::None
        }
    }

    fn history_previous(&mut self) -> bool {
        let selected = PROMPT_HISTORY.with(|history| {
            let mut history = history.borrow_mut();
            if history.entries.is_empty() {
                return None;
            }
            let position = match history.position {
                Some(position) => position.saturating_sub(1),
                None => {
                    history.saved_draft = Some(self.draft.clone());
                    history.entries.len() - 1
                }
            };
            history.position = Some(position);
            history.entries.get(position).cloned()
        });
        selected.is_some_and(|draft| self.replace_from_history(draft))
    }

    fn history_next(&mut self) -> bool {
        let selected = PROMPT_HISTORY.with(|history| {
            let mut history = history.borrow_mut();
            let position = history.position?;
            if position + 1 < history.entries.len() {
                let next = position + 1;
                history.position = Some(next);
                history.entries.get(next).cloned()
            } else {
                history.position = None;
                history.saved_draft.take()
            }
        });
        selected.is_some_and(|draft| self.replace_from_history(draft))
    }

    fn replace_from_history(&mut self, mut draft: PromptDraft) -> bool {
        let revision = self.draft.revision.saturating_add(1);
        draft.revision = revision;
        self.cursor = draft.text.len();
        self.draft = draft;
        true
    }
}

fn record_history(draft: &PromptDraft) {
    PROMPT_HISTORY.with(|history| {
        let mut history = history.borrow_mut();
        history.position = None;
        history.saved_draft = None;
        if history.entries.last() == Some(draft) {
            return;
        }
        history.entries.push(draft.clone());
        if history.entries.len() > MAX_PROMPT_HISTORY {
            let overflow = history.entries.len() - MAX_PROMPT_HISTORY;
            history.entries.drain(..overflow);
        }
    });
}

fn reset_history_navigation() {
    PROMPT_HISTORY.with(|history| {
        let mut history = history.borrow_mut();
        history.position = None;
        history.saved_draft = None;
    });
}

fn line_start(text: &str, cursor: usize) -> usize {
    text[..cursor.min(text.len())]
        .rfind('\n')
        .map_or(0, |index| index + 1)
}

fn line_end(text: &str, cursor: usize) -> usize {
    let cursor = cursor.min(text.len());
    text[cursor..]
        .find('\n')
        .map_or(text.len(), |offset| cursor + offset)
}

fn previous_word_boundary(text: &str, cursor: usize) -> usize {
    let mut current = cursor.min(text.len());
    while current > 0 {
        let previous = previous_grapheme_boundary(text, current);
        if !grapheme_is_whitespace(&text[previous..current]) {
            break;
        }
        current = previous;
    }
    if current == 0 {
        return 0;
    }
    let previous = previous_grapheme_boundary(text, current);
    let class = grapheme_word_class(&text[previous..current]);
    current = previous;
    while current > 0 {
        let candidate = previous_grapheme_boundary(text, current);
        if grapheme_word_class(&text[candidate..current]) != class {
            break;
        }
        current = candidate;
    }
    current
}

fn next_word_boundary(text: &str, cursor: usize) -> usize {
    let mut current = cursor.min(text.len());
    while current < text.len() {
        let next = next_grapheme_boundary(text, current);
        if !grapheme_is_whitespace(&text[current..next]) {
            break;
        }
        current = next;
    }
    if current >= text.len() {
        return text.len();
    }
    let next = next_grapheme_boundary(text, current);
    let class = grapheme_word_class(&text[current..next]);
    current = next;
    while current < text.len() {
        let candidate = next_grapheme_boundary(text, current);
        if grapheme_word_class(&text[current..candidate]) != class {
            break;
        }
        current = candidate;
    }
    while current < text.len() {
        let next = next_grapheme_boundary(text, current);
        if !grapheme_is_whitespace(&text[current..next]) {
            break;
        }
        current = next;
    }
    current
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WordClass {
    Word,
    Punctuation,
    Whitespace,
}

fn grapheme_word_class(grapheme: &str) -> WordClass {
    let first = grapheme.chars().next().unwrap_or(' ');
    if first.is_whitespace() {
        WordClass::Whitespace
    } else if first.is_alphanumeric() || first == '_' {
        WordClass::Word
    } else {
        WordClass::Punctuation
    }
}

fn grapheme_is_whitespace(grapheme: &str) -> bool {
    grapheme_word_class(grapheme) == WordClass::Whitespace
}

fn previous_grapheme_boundary(text: &str, cursor: usize) -> usize {
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

fn next_grapheme_boundary(text: &str, cursor: usize) -> usize {
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

fn grapheme_count(text: &str) -> usize {
    let mut count = 0;
    let mut cursor = 0;
    while cursor < text.len() {
        cursor = next_grapheme_boundary(text, cursor);
        count += 1;
    }
    count
}

fn grapheme_boundary_at_column(text: &str, column: usize) -> usize {
    let mut cursor = 0;
    for _ in 0..column {
        if cursor >= text.len() {
            break;
        }
        cursor = next_grapheme_boundary(text, cursor);
    }
    cursor
}

fn normalized_len(text: &str) -> usize {
    text.replace("\r\n", "\n").replace('\r', "\n").len()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> Event {
        Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    fn modified_key(code: KeyCode, modifiers: KeyModifiers) -> Event {
        Event::Key(KeyEvent::new(code, modifiers))
    }

    fn clear_history() {
        PROMPT_HISTORY.with(|history| *history.borrow_mut() = PromptHistory::default());
    }

    #[test]
    fn backspace_and_arrows_use_grapheme_boundaries() {
        clear_history();
        let mut composer = ComposerState::new("A👨‍👩‍👧‍👦e\u{301}界");
        let end = composer.cursor;
        composer.handle_event(key(KeyCode::Left)).expect("left cjk");
        let after_cjk = composer.cursor;
        assert!(after_cjk < end);
        composer
            .handle_event(key(KeyCode::Left))
            .expect("left combining");
        let after_combining = composer.cursor;
        assert_eq!(&composer.draft.text[after_combining..after_cjk], "e\u{301}");
        composer
            .handle_event(key(KeyCode::Left))
            .expect("left emoji");
        let emoji_start = composer.cursor;
        assert_eq!(&composer.draft.text[emoji_start..after_combining], "👨‍👩‍👧‍👦");

        composer.cursor = after_combining;
        composer
            .handle_event(key(KeyCode::Backspace))
            .expect("backspace emoji");
        assert_eq!(composer.draft.text, "Ae\u{301}界");
        assert_eq!(composer.cursor, 1);
    }

    #[test]
    fn right_arrow_crosses_combining_and_flag_clusters_once() {
        clear_history();
        let mut composer = ComposerState::new("e\u{301}🇨🇭界");
        composer.cursor = 0;
        composer
            .handle_event(key(KeyCode::Right))
            .expect("combining");
        assert_eq!(&composer.draft.text[..composer.cursor], "e\u{301}");
        composer.handle_event(key(KeyCode::Right)).expect("flag");
        assert_eq!(&composer.draft.text[..composer.cursor], "e\u{301}🇨🇭");
        composer.handle_event(key(KeyCode::Right)).expect("cjk");
        assert_eq!(composer.cursor, composer.draft.text.len());
    }

    #[test]
    fn home_end_and_delete_are_grapheme_safe_in_multiline_text() {
        clear_history();
        let mut composer = ComposerState::new("first\ne\u{301}👩‍💻界\nlast");
        composer.cursor = composer.draft.text.find('界').expect("cjk");

        composer.handle_event(key(KeyCode::Home)).expect("home");
        assert_eq!(&composer.draft.text[..composer.cursor], "first\n");
        composer.handle_event(key(KeyCode::End)).expect("end");
        assert_eq!(
            &composer.draft.text[..composer.cursor],
            "first\ne\u{301}👩‍💻界"
        );

        composer.cursor = "first\n".len();
        assert_eq!(
            composer
                .handle_event(key(KeyCode::Delete))
                .expect("delete combining"),
            ComposerAction::Changed
        );
        assert_eq!(composer.draft.text, "first\n👩‍💻界\nlast");
        assert_eq!(composer.cursor, "first\n".len());
        assert_eq!(
            composer
                .handle_event(key(KeyCode::Delete))
                .expect("delete emoji"),
            ComposerAction::Changed
        );
        assert_eq!(composer.draft.text, "first\n界\nlast");

        composer
            .handle_event(modified_key(KeyCode::End, KeyModifiers::CONTROL))
            .expect("document end");
        assert_eq!(composer.cursor, composer.draft.text.len());
        composer
            .handle_event(modified_key(KeyCode::Home, KeyModifiers::CONTROL))
            .expect("document home");
        assert_eq!(composer.cursor, 0);
    }

    #[test]
    fn control_arrows_move_by_unicode_word_boundaries() {
        clear_history();
        let mut composer = ComposerState::new("alpha e\u{301}clair 👩‍💻 界_beta");
        composer.cursor = composer.draft.text.len();

        composer
            .handle_event(modified_key(KeyCode::Left, KeyModifiers::CONTROL))
            .expect("left word");
        assert_eq!(&composer.draft.text[composer.cursor..], "界_beta");
        composer
            .handle_event(modified_key(KeyCode::Left, KeyModifiers::CONTROL))
            .expect("left emoji");
        assert_eq!(&composer.draft.text[composer.cursor..], "👩‍💻 界_beta");
        composer
            .handle_event(modified_key(KeyCode::Right, KeyModifiers::CONTROL))
            .expect("right emoji");
        assert_eq!(&composer.draft.text[composer.cursor..], "界_beta");
        composer
            .handle_event(modified_key(KeyCode::Right, KeyModifiers::CONTROL))
            .expect("right word");
        assert_eq!(composer.cursor, composer.draft.text.len());
    }

    #[test]
    fn up_down_move_within_multiline_before_history() {
        clear_history();
        let mut composer = ComposerState::new("a👩‍💻\nb\u{301}c\n界z");
        composer.cursor = composer.draft.text.find("界").expect("third line") + "界".len();
        assert_eq!(
            composer.handle_event(key(KeyCode::Up)).expect("up"),
            ComposerAction::None
        );
        let second_line = composer.draft.text.find("b\u{301}c").expect("second line");
        assert_eq!(composer.cursor, second_line + "b\u{301}".len());
        assert_eq!(
            composer.handle_event(key(KeyCode::Down)).expect("down"),
            ComposerAction::None
        );
        assert_eq!(
            &composer.draft.text[..composer.cursor],
            "a👩‍💻\nb\u{301}c\n界"
        );
    }

    #[test]
    fn history_navigation_only_starts_at_vertical_boundaries() {
        clear_history();
        let mut first = ComposerState::new("first");
        assert_eq!(
            first
                .handle_event(key(KeyCode::Enter))
                .expect("submit first"),
            ComposerAction::Submit
        );
        let mut second = ComposerState::new("second");
        assert_eq!(
            second
                .handle_event(key(KeyCode::Enter))
                .expect("submit second"),
            ComposerAction::Submit
        );

        let mut composer = ComposerState::new("draft\nline");
        composer.cursor = composer.draft.text.len();
        assert_eq!(
            composer
                .handle_event(key(KeyCode::Up))
                .expect("vertical up"),
            ComposerAction::None
        );
        assert_eq!(composer.draft.text, "draft\nline");
        composer.cursor = 0;
        assert_eq!(
            composer.handle_event(key(KeyCode::Up)).expect("history up"),
            ComposerAction::Changed
        );
        assert_eq!(composer.draft.text, "second");
        assert_eq!(
            composer.handle_event(key(KeyCode::Up)).expect("older"),
            ComposerAction::Changed
        );
        assert_eq!(composer.draft.text, "first");
        assert_eq!(
            composer.handle_event(key(KeyCode::Down)).expect("newer"),
            ComposerAction::Changed
        );
        assert_eq!(composer.draft.text, "second");
        assert_eq!(
            composer.handle_event(key(KeyCode::Down)).expect("restore"),
            ComposerAction::Changed
        );
        assert_eq!(composer.draft.text, "draft\nline");
    }

    #[test]
    fn slash_completion_keeps_up_down_for_command_selection() {
        clear_history();
        let mut composer = ComposerState::new("/mo");
        assert_eq!(
            composer.handle_event(key(KeyCode::Up)).expect("up"),
            ComposerAction::CommandPrevious
        );
        assert_eq!(
            composer.handle_event(key(KeyCode::Down)).expect("down"),
            ComposerAction::CommandNext
        );
    }

    #[test]
    fn paste_normalization_keeps_cursor_on_final_boundary() {
        clear_history();
        let mut composer = ComposerState::new("界");
        composer
            .handle_event(Event::Paste("e\u{301}\r\n👩‍💻".to_owned()))
            .expect("paste");
        assert_eq!(composer.draft.text, "界e\u{301}\n👩‍💻");
        assert_eq!(composer.cursor, composer.draft.text.len());
    }
}
