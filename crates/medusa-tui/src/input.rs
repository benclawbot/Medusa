use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::clipboard::{ClipboardError, PromptDraft};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ComposerAction {
    None,
    Changed,
    Submit,
    Interrupt,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ComposerState {
    pub draft: PromptDraft,
    pub cursor: usize,
}

impl ComposerState {
    #[must_use]
    pub fn new(initial_text: impl Into<String>) -> Self {
        let text = initial_text.into();
        let cursor = text.len();
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
                Ok(ComposerAction::Changed)
            }
            Event::Key(key) if key.kind == KeyEventKind::Press => self.handle_key(key),
            _ => Ok(ComposerAction::None),
        }
    }

    fn handle_key(&mut self, key: KeyEvent) -> Result<ComposerAction, ClipboardError> {
        match (key.code, key.modifiers) {
            (KeyCode::Enter, KeyModifiers::NONE) => {
                if self.draft.text.trim().is_empty() && self.draft.attachments.is_empty() {
                    Ok(ComposerAction::None)
                } else {
                    Ok(ComposerAction::Submit)
                }
            }
            (KeyCode::Enter, modifiers) if modifiers.contains(KeyModifiers::SHIFT) => {
                self.insert_text("\n")
            }
            (KeyCode::Char('c'), modifiers) if modifiers.contains(KeyModifiers::CONTROL) => {
                Ok(ComposerAction::Interrupt)
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
                let previous = self.draft.text[..self.cursor]
                    .char_indices()
                    .next_back()
                    .map(|(index, _)| index)
                    .unwrap_or(0);
                self.draft.text.replace_range(previous..self.cursor, "");
                self.cursor = previous;
                self.draft.revision = self.draft.revision.saturating_add(1);
                Ok(ComposerAction::Changed)
            }
            (KeyCode::Left, _) => {
                self.cursor = self.draft.text[..self.cursor]
                    .char_indices()
                    .next_back()
                    .map(|(index, _)| index)
                    .unwrap_or(0);
                Ok(ComposerAction::None)
            }
            (KeyCode::Right, _) => {
                self.cursor = self.draft.text[self.cursor..]
                    .char_indices()
                    .nth(1)
                    .map(|(offset, _)| self.cursor + offset)
                    .unwrap_or(self.draft.text.len());
                Ok(ComposerAction::None)
            }
            _ => Ok(ComposerAction::None),
        }
    }

    fn insert_text(&mut self, text: &str) -> Result<ComposerAction, ClipboardError> {
        self.draft.insert_pasted_text(self.cursor, text)?;
        self.cursor += text.len();
        Ok(ComposerAction::Changed)
    }
}

fn normalized_len(text: &str) -> usize {
    text.replace("\r\n", "\n").replace('\r', "\n").len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bracketed_paste_never_submits() {
        let mut composer = ComposerState::new("");
        let action = composer
            .handle_event(Event::Paste("cargo test\nrm -rf /".to_owned()))
            .expect("handle paste");
        assert_eq!(action, ComposerAction::Changed);
        assert_eq!(composer.draft.text, "cargo test\nrm -rf /");
    }

    #[test]
    fn pasted_crlf_updates_cursor_using_normalized_length() {
        let mut composer = ComposerState::new("a");
        composer
            .handle_event(Event::Paste("b\r\nc".to_owned()))
            .expect("handle paste");
        assert_eq!(composer.draft.text, "ab\nc");
        assert_eq!(composer.cursor, composer.draft.text.len());
    }

    #[test]
    fn enter_submits_non_empty_draft() {
        let mut composer = ComposerState::new("fix tests");
        let action = composer
            .handle_event(Event::Key(KeyEvent::new(
                KeyCode::Enter,
                KeyModifiers::NONE,
            )))
            .expect("handle enter");
        assert_eq!(action, ComposerAction::Submit);
    }

    #[test]
    fn unicode_backspace_removes_one_scalar() {
        let mut composer = ComposerState::new("aé");
        composer
            .handle_event(Event::Key(KeyEvent::new(
                KeyCode::Backspace,
                KeyModifiers::NONE,
            )))
            .expect("handle backspace");
        assert_eq!(composer.draft.text, "a");
        assert_eq!(composer.cursor, 1);
    }
}
