use std::io::{self, IsTerminal, Write};

use crossterm::{
    cursor::{Hide, MoveUp, Show},
    event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
    execute, queue,
    style::{Attribute, Print, SetAttribute},
    terminal::{Clear, ClearType, disable_raw_mode, enable_raw_mode},
};

use crate::clipboard::{ClipboardError, PromptDraft};

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
                    Ok(ComposerAction::Submit)
                }
            }
            (KeyCode::Enter, modifiers) if modifiers.contains(KeyModifiers::SHIFT) => {
                self.insert_text("\n")
            }
            (KeyCode::Char('c'), modifiers) if modifiers.contains(KeyModifiers::CONTROL) => {
                Ok(ComposerAction::Interrupt)
            }
            (KeyCode::Up, _) => Ok(ComposerAction::CommandPrevious),
            (KeyCode::Down, _) => Ok(ComposerAction::CommandNext),
            (KeyCode::Tab, _) => Ok(ComposerAction::CompleteCommand),
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

/// Reusable keyboard-first selection state shared by terminal menus and modal pickers.
///
/// The state keeps selection, search text, and scroll position independent from rendering so
/// callers can preserve a highlighted parent row while navigating nested screens.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SelectionState {
    selected: usize,
    search: String,
    scroll: usize,
}

impl Default for SelectionState {
    fn default() -> Self {
        Self::new(0)
    }
}

impl SelectionState {
    #[must_use]
    pub const fn new(selected: usize) -> Self {
        Self {
            selected,
            search: String::new(),
            scroll: 0,
        }
    }

    #[must_use]
    pub const fn selected(&self) -> usize {
        self.selected
    }

    pub fn set_selected(&mut self, selected: usize, count: usize) {
        self.selected = if count == 0 {
            0
        } else {
            selected.min(count.saturating_sub(1))
        };
    }

    pub fn move_by(&mut self, count: usize, delta: isize) {
        self.move_by_with(count, delta, |_| true);
    }

    pub fn move_by_with(
        &mut self,
        count: usize,
        delta: isize,
        mut enabled: impl FnMut(usize) -> bool,
    ) {
        if count == 0 || delta == 0 || !(0..count).any(&mut enabled) {
            return;
        }
        let direction = delta.signum();
        let mut candidate = self.selected.min(count.saturating_sub(1));
        for _ in 0..count {
            candidate = (candidate as isize + direction).rem_euclid(count as isize) as usize;
            if enabled(candidate) {
                self.selected = candidate;
                return;
            }
        }
    }

    #[must_use]
    pub fn search(&self) -> &str {
        &self.search
    }

    pub fn push_search(&mut self, character: char) {
        self.search.push(character);
        self.scroll = 0;
    }

    pub fn pop_search(&mut self) {
        self.search.pop();
        self.scroll = 0;
    }

    pub fn clear_search(&mut self) {
        self.search.clear();
        self.scroll = 0;
    }

    #[must_use]
    pub fn filtered_indices<T: AsRef<str>>(&self, labels: &[T]) -> Vec<usize> {
        let query = self.search.trim().to_lowercase();
        labels
            .iter()
            .enumerate()
            .filter_map(|(index, label)| {
                (query.is_empty() || label.as_ref().to_lowercase().contains(&query))
                    .then_some(index)
            })
            .collect()
    }

    pub fn ensure_visible(&mut self, ordered_indices: &[usize], visible_rows: usize) {
        if ordered_indices.is_empty() || visible_rows == 0 {
            self.scroll = 0;
            return;
        }
        let position = ordered_indices
            .iter()
            .position(|index| *index == self.selected)
            .unwrap_or(0);
        if position < self.scroll {
            self.scroll = position;
        } else if position >= self.scroll.saturating_add(visible_rows) {
            self.scroll = position.saturating_add(1).saturating_sub(visible_rows);
        }
        self.scroll = self.scroll.min(
            ordered_indices
                .len()
                .saturating_sub(visible_rows.min(ordered_indices.len())),
        );
    }

    #[must_use]
    pub fn visible_indices(&self, ordered_indices: &[usize], visible_rows: usize) -> Vec<usize> {
        ordered_indices
            .iter()
            .skip(self.scroll)
            .take(visible_rows)
            .copied()
            .collect()
    }
}

/// Runs a compact keyboard-first terminal picker.
///
/// The caller must provide an interactive terminal. `None` means the user cancelled with Escape
/// or Ctrl+C. Raw mode and cursor visibility are restored before this function returns.
pub fn select_menu(title: &str, choices: &[&str], initial: usize) -> io::Result<Option<usize>> {
    if choices.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "terminal menu requires at least one choice",
        ));
    }
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "terminal menu requires interactive stdin and stdout",
        ));
    }

    let mut terminal = MenuTerminal::enter()?;
    let mut selection = SelectionState::new(initial.min(choices.len() - 1));
    loop {
        terminal.render(title, choices, selection.selected())?;
        let Event::Key(key) = event::read()? else {
            continue;
        };
        if key.kind == KeyEventKind::Release {
            continue;
        }
        match menu_action(key, selection.selected(), choices.len()) {
            MenuAction::Move(next) => selection.set_selected(next, choices.len()),
            MenuAction::Select => {
                terminal.complete(title, choices[selection.selected()])?;
                return Ok(Some(selection.selected()));
            }
            MenuAction::Cancel => {
                terminal.cancel(title)?;
                return Ok(None);
            }
            MenuAction::Ignore => {}
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MenuAction {
    Move(usize),
    Select,
    Cancel,
    Ignore,
}

fn menu_action(key: KeyEvent, selected: usize, count: usize) -> MenuAction {
    if count == 0 {
        return MenuAction::Ignore;
    }
    match key.code {
        KeyCode::Up => MenuAction::Move(if selected == 0 {
            count - 1
        } else {
            selected - 1
        }),
        KeyCode::Down => MenuAction::Move((selected + 1) % count),
        KeyCode::Home => MenuAction::Move(0),
        KeyCode::End => MenuAction::Move(count - 1),
        KeyCode::Enter => MenuAction::Select,
        KeyCode::Esc => MenuAction::Cancel,
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => MenuAction::Cancel,
        _ => MenuAction::Ignore,
    }
}

struct MenuTerminal {
    rendered_rows: u16,
    restored: bool,
}

impl MenuTerminal {
    fn enter() -> io::Result<Self> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        if let Err(error) = execute!(stdout, Hide) {
            let _ = disable_raw_mode();
            return Err(error);
        }
        Ok(Self {
            rendered_rows: 0,
            restored: false,
        })
    }

    fn render(&mut self, title: &str, choices: &[&str], selected: usize) -> io::Result<()> {
        let mut stdout = io::stdout();
        self.clear_rendered(&mut stdout)?;
        queue!(
            stdout,
            Print(title),
            Print(":\r\n"),
            Print("  Up/Down move  Enter select  Esc cancel\r\n")
        )?;
        for (index, label) in choices.iter().enumerate() {
            let marker = if index == selected { ">" } else { " " };
            if index == selected {
                queue!(stdout, SetAttribute(Attribute::Bold))?;
            }
            queue!(stdout, Print(format!("  {marker} {label}\r\n")))?;
            if index == selected {
                queue!(stdout, SetAttribute(Attribute::Reset))?;
            }
        }
        stdout.flush()?;
        self.rendered_rows = u16::try_from(choices.len().saturating_add(2)).unwrap_or(u16::MAX);
        Ok(())
    }

    fn complete(&mut self, title: &str, selected: &str) -> io::Result<()> {
        let mut stdout = io::stdout();
        self.clear_rendered(&mut stdout)?;
        queue!(stdout, Print(format!("{title}: {selected}\r\n")))?;
        stdout.flush()?;
        self.restore()
    }

    fn cancel(&mut self, title: &str) -> io::Result<()> {
        let mut stdout = io::stdout();
        self.clear_rendered(&mut stdout)?;
        queue!(stdout, Print(format!("{title}: cancelled\r\n")))?;
        stdout.flush()?;
        self.restore()
    }

    fn clear_rendered(&mut self, stdout: &mut io::Stdout) -> io::Result<()> {
        if self.rendered_rows > 0 {
            queue!(
                stdout,
                MoveUp(self.rendered_rows),
                Clear(ClearType::FromCursorDown)
            )?;
        }
        Ok(())
    }

    fn restore(&mut self) -> io::Result<()> {
        if self.restored {
            return Ok(());
        }
        let raw_result = disable_raw_mode();
        let mut stdout = io::stdout();
        let cursor_result = execute!(stdout, SetAttribute(Attribute::Reset), Show);
        self.restored = true;
        raw_result.and(cursor_result)
    }
}

impl Drop for MenuTerminal {
    fn drop(&mut self) {
        let _ = self.restore();
    }
}

fn normalized_len(text: &str) -> usize {
    text.replace("\r\n", "\n").replace('\r', "\n").len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clipboard::{FileAttachment, PromptAttachment};
    use std::path::PathBuf;

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
    fn empty_enter_is_ignored_but_attachment_only_prompt_submits() {
        let mut composer = ComposerState::new("   ");
        assert_eq!(
            composer
                .handle_event(Event::Key(KeyEvent::new(
                    KeyCode::Enter,
                    KeyModifiers::NONE,
                )))
                .expect("empty enter"),
            ComposerAction::None
        );
        composer
            .draft
            .attachments
            .push(PromptAttachment::File(FileAttachment {
                path: PathBuf::from("context.txt"),
                byte_len: 3,
            }));
        assert_eq!(
            composer
                .handle_event(Event::Key(KeyEvent::new(
                    KeyCode::Enter,
                    KeyModifiers::NONE,
                )))
                .expect("attachment enter"),
            ComposerAction::Submit
        );
    }

    #[test]
    fn shift_enter_inserts_newline_and_ctrl_c_interrupts() {
        let mut composer = ComposerState::new("line one");
        assert_eq!(
            composer
                .handle_event(Event::Key(KeyEvent::new(
                    KeyCode::Enter,
                    KeyModifiers::SHIFT,
                )))
                .expect("shift enter"),
            ComposerAction::Changed
        );
        assert_eq!(composer.draft.text, "line one\n");
        assert_eq!(
            composer
                .handle_event(Event::Key(KeyEvent::new(
                    KeyCode::Char('c'),
                    KeyModifiers::CONTROL,
                )))
                .expect("ctrl c"),
            ComposerAction::Interrupt
        );
    }

    #[test]
    fn navigation_and_tab_are_forwarded_for_command_completion() {
        let mut composer = ComposerState::new("/");
        for (key, expected) in [
            (KeyCode::Up, ComposerAction::CommandPrevious),
            (KeyCode::Down, ComposerAction::CommandNext),
            (KeyCode::Tab, ComposerAction::CompleteCommand),
        ] {
            assert_eq!(
                composer
                    .handle_event(Event::Key(KeyEvent::new(key, KeyModifiers::NONE)))
                    .expect("handle command navigation"),
                expected
            );
        }
    }

    #[test]
    fn character_input_and_cursor_navigation_are_unicode_safe() {
        let mut composer = ComposerState::new("aé");
        assert_eq!(
            composer
                .handle_event(Event::Key(
                    KeyEvent::new(KeyCode::Left, KeyModifiers::NONE,)
                ))
                .expect("left"),
            ComposerAction::None
        );
        assert_eq!(composer.cursor, 1);
        assert_eq!(
            composer
                .handle_event(Event::Key(KeyEvent::new(
                    KeyCode::Char('Z'),
                    KeyModifiers::SHIFT,
                )))
                .expect("character"),
            ComposerAction::Changed
        );
        assert_eq!(composer.draft.text, "aZé");
        assert_eq!(
            composer
                .handle_event(Event::Key(KeyEvent::new(
                    KeyCode::Right,
                    KeyModifiers::NONE,
                )))
                .expect("right"),
            ComposerAction::None
        );
        assert_eq!(composer.cursor, composer.draft.text.len());
    }

    #[test]
    fn boundary_navigation_and_non_press_events_are_noops() {
        let mut composer = ComposerState::new("x");
        composer.cursor = 0;
        assert_eq!(
            composer
                .handle_event(Event::Key(KeyEvent::new(
                    KeyCode::Backspace,
                    KeyModifiers::NONE,
                )))
                .expect("backspace at start"),
            ComposerAction::None
        );
        assert_eq!(
            composer
                .handle_event(Event::Key(
                    KeyEvent::new(KeyCode::Left, KeyModifiers::NONE,)
                ))
                .expect("left at start"),
            ComposerAction::None
        );
        let mut release = KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE);
        release.kind = KeyEventKind::Release;
        assert_eq!(
            composer.handle_event(Event::Key(release)).expect("release"),
            ComposerAction::None
        );
        assert_eq!(composer.draft.text, "x");
    }

    #[test]
    fn repeated_character_events_are_kept_while_key_releases_are_ignored() {
        let mut composer = ComposerState::new("");
        let mut repeat = KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE);
        repeat.kind = KeyEventKind::Repeat;
        assert_eq!(
            composer.handle_event(Event::Key(repeat)).expect("repeat"),
            ComposerAction::Changed
        );
        assert_eq!(composer.draft.text, "n");
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

    #[test]
    fn spacebar_is_inserted_as_regular_text() {
        let mut composer = ComposerState::new("hello");
        composer
            .handle_event(Event::Key(KeyEvent::new(
                KeyCode::Char(' '),
                KeyModifiers::NONE,
            )))
            .expect("spacebar");
        composer
            .handle_event(Event::Key(KeyEvent::new(
                KeyCode::Char('w'),
                KeyModifiers::NONE,
            )))
            .expect("word");
        assert_eq!(composer.draft.text, "hello w");
    }

    #[test]
    fn menu_navigation_wraps_and_supports_home_end() {
        assert_eq!(
            menu_action(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE), 0, 4),
            MenuAction::Move(3)
        );
        assert_eq!(
            menu_action(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE), 3, 4),
            MenuAction::Move(0)
        );
        assert_eq!(
            menu_action(KeyEvent::new(KeyCode::Home, KeyModifiers::NONE), 2, 4),
            MenuAction::Move(0)
        );
        assert_eq!(
            menu_action(KeyEvent::new(KeyCode::End, KeyModifiers::NONE), 1, 4),
            MenuAction::Move(3)
        );
    }

    #[test]
    fn menu_enter_selects_and_escape_or_ctrl_c_cancel() {
        assert_eq!(
            menu_action(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), 1, 3),
            MenuAction::Select
        );
        assert_eq!(
            menu_action(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), 1, 3),
            MenuAction::Cancel
        );
        assert_eq!(
            menu_action(
                KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
                1,
                3
            ),
            MenuAction::Cancel
        );
    }

    #[test]
    fn selection_state_skips_disabled_rows_and_preserves_parent_selection() {
        let mut state = SelectionState::new(1);
        state.move_by_with(4, 1, |index| index != 2);
        assert_eq!(state.selected(), 3);
        state.move_by_with(4, -1, |index| index != 2);
        assert_eq!(state.selected(), 1);
    }

    #[test]
    fn selection_state_filters_and_scrolls_long_lists() {
        let labels = ["alpha", "beta", "alphabet", "gamma"];
        let mut state = SelectionState::new(2);
        for character in "alp".chars() {
            state.push_search(character);
        }
        let filtered = state.filtered_indices(&labels);
        assert_eq!(filtered, vec![0, 2]);
        state.ensure_visible(&filtered, 1);
        assert_eq!(state.visible_indices(&filtered, 1), vec![2]);
        state.clear_search();
        assert!(state.search().is_empty());
    }
}
