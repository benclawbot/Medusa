use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};

use super::*;

impl AppState {
    pub(super) fn handle_model_modal_event(&mut self, event: Event) -> Result<AppAction, AppError> {
        match event {
            Event::Paste(text) => {
                if let Some(modal) = self.model_modal.as_mut() {
                    modal.focus_api_key();
                    modal.insert_key_text(&text);
                }
                Ok(AppAction::Redraw)
            }
            Event::Key(key) if key.kind != KeyEventKind::Release => match key.code {
                KeyCode::Esc => {
                    self.model_modal = None;
                    self.status = "model configuration cancelled".to_owned();
                    Ok(AppAction::Redraw)
                }
                KeyCode::Enter => {
                    let submit = self
                        .model_modal
                        .as_ref()
                        .is_some_and(|modal| modal.focus() == ModelModalFocus::Apply);
                    if submit {
                        if let Some(modal) = self.model_modal.take() {
                            Ok(AppAction::ConfigureModel(modal.configuration()))
                        } else {
                            Ok(AppAction::None)
                        }
                    } else {
                        if let Some(modal) = self.model_modal.as_mut() {
                            modal.cycle_focus();
                        }
                        Ok(AppAction::Redraw)
                    }
                }
                KeyCode::BackTab => {
                    if let Some(modal) = self.model_modal.as_mut() {
                        modal.cycle_focus_back();
                    }
                    Ok(AppAction::Redraw)
                }
                KeyCode::Tab if key.modifiers.contains(KeyModifiers::SHIFT) => {
                    if let Some(modal) = self.model_modal.as_mut() {
                        modal.cycle_focus_back();
                    }
                    Ok(AppAction::Redraw)
                }
                KeyCode::Tab => {
                    if let Some(modal) = self.model_modal.as_mut() {
                        modal.cycle_focus();
                    }
                    Ok(AppAction::Redraw)
                }
                KeyCode::Up | KeyCode::Left => {
                    if let Some(modal) = self.model_modal.as_mut() {
                        modal.move_selection(-1);
                    }
                    Ok(AppAction::Redraw)
                }
                KeyCode::Down | KeyCode::Right => {
                    if let Some(modal) = self.model_modal.as_mut() {
                        modal.move_selection(1);
                    }
                    Ok(AppAction::Redraw)
                }
                KeyCode::Backspace => {
                    if let Some(modal) = self.model_modal.as_mut()
                        && modal.focus() == ModelModalFocus::ApiKey
                    {
                        modal.delete_key_character();
                    }
                    Ok(AppAction::Redraw)
                }
                KeyCode::Char('v') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    let clipboard = self.clipboard.read()?;
                    if let ClipboardContent::Text(text) = clipboard
                        && let Some(modal) = self.model_modal.as_mut()
                    {
                        modal.focus_api_key();
                        modal.insert_key_text(&text);
                    }
                    Ok(AppAction::Redraw)
                }
                KeyCode::Char(character)
                    if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT =>
                {
                    if let Some(modal) = self.model_modal.as_mut()
                        && modal.focus() == ModelModalFocus::ApiKey
                    {
                        modal.insert_key_text(&character.to_string());
                    }
                    Ok(AppAction::Redraw)
                }
                _ => Ok(AppAction::None),
            },
            _ => Ok(AppAction::None),
        }
    }

    pub(super) fn handle_question_modal_event(
        &mut self,
        event: Event,
    ) -> Result<AppAction, AppError> {
        match event {
            Event::Paste(text) => {
                if let Some(modal) = self.question_modal.as_mut() {
                    modal.insert_answer(&text);
                }
                Ok(AppAction::Redraw)
            }
            Event::Key(key) if key.kind != KeyEventKind::Release => match key.code {
                KeyCode::Esc => {
                    if let Some(modal) = self.question_modal.as_mut()
                        && modal.is_reviewing()
                    {
                        modal.move_question(-1);
                    }
                    self.status = "waiting for your answers".to_owned();
                    Ok(AppAction::Redraw)
                }
                KeyCode::BackTab => {
                    if let Some(modal) = self.question_modal.as_mut() {
                        modal.move_question(-1);
                    }
                    Ok(AppAction::Redraw)
                }
                KeyCode::Tab if key.modifiers.contains(KeyModifiers::SHIFT) => {
                    if let Some(modal) = self.question_modal.as_mut() {
                        modal.move_question(-1);
                    }
                    Ok(AppAction::Redraw)
                }
                KeyCode::Tab => {
                    if let Some(modal) = self.question_modal.as_mut() {
                        modal.move_question(1);
                    }
                    Ok(AppAction::Redraw)
                }
                KeyCode::Up | KeyCode::Left => {
                    if let Some(modal) = self.question_modal.as_mut()
                        && !modal.is_reviewing()
                    {
                        modal.move_selection(-1);
                    }
                    Ok(AppAction::Redraw)
                }
                KeyCode::Down | KeyCode::Right => {
                    if let Some(modal) = self.question_modal.as_mut()
                        && !modal.is_reviewing()
                    {
                        modal.move_selection(1);
                    }
                    Ok(AppAction::Redraw)
                }
                KeyCode::Backspace => {
                    if let Some(modal) = self.question_modal.as_mut()
                        && !modal.is_reviewing()
                    {
                        modal.delete_answer_character();
                    }
                    Ok(AppAction::Redraw)
                }
                KeyCode::Char('v') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    if let ClipboardContent::Text(text) = self.clipboard.read()?
                        && let Some(modal) = self.question_modal.as_mut()
                        && !modal.is_reviewing()
                    {
                        modal.insert_answer(&text);
                    }
                    Ok(AppAction::Redraw)
                }
                KeyCode::Char(' ') if key.modifiers.is_empty() => {
                    if let Some(modal) = self.question_modal.as_mut()
                        && !modal.is_reviewing()
                    {
                        let is_multi_select = modal.active_prompt().is_some_and(|prompt| {
                            prompt.multi_select && !prompt.options.is_empty()
                        });
                        if is_multi_select {
                            modal.toggle_current_option();
                        } else {
                            modal.insert_answer(" ");
                        }
                    }
                    Ok(AppAction::Redraw)
                }
                KeyCode::Char(character)
                    if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT =>
                {
                    if let Some(modal) = self.question_modal.as_mut()
                        && !modal.is_reviewing()
                    {
                        modal.insert_answer(&character.to_string());
                    }
                    Ok(AppAction::Redraw)
                }
                KeyCode::Enter => {
                    let reviewing = self
                        .question_modal
                        .as_ref()
                        .is_some_and(QuestionModal::is_reviewing);
                    if reviewing {
                        return self.submit_question_answers();
                    }

                    let is_last_question = self.question_modal.as_ref().is_some_and(|modal| {
                        modal.active_question().saturating_add(1) >= modal.questions().len()
                    });
                    if let Some(modal) = self.question_modal.as_mut() {
                        if !modal.select_current_answer() {
                            self.status = "choose or type an answer".to_owned();
                            return Ok(AppAction::Redraw);
                        }
                        if !is_last_question {
                            modal.advance_or_review();
                            return Ok(AppAction::Redraw);
                        }
                    } else {
                        return Ok(AppAction::None);
                    }
                    self.submit_question_answers()
                }
                _ => Ok(AppAction::None),
            },
            _ => Ok(AppAction::None),
        }
    }

    fn submit_question_answers(&mut self) -> Result<AppAction, AppError> {
        let Some(answer) = self
            .question_modal
            .as_ref()
            .and_then(QuestionModal::answer_bundle)
        else {
            self.status = "answer every question before continuing".to_owned();
            return Ok(AppAction::Redraw);
        };
        self.question_modal = None;
        self.transcript.push(TranscriptEntry::User(PromptDraft {
            text: answer.clone(),
            ..PromptDraft::default()
        }));
        self.status = "answers submitted".to_owned();
        Ok(AppAction::AnswerQuestion(answer))
    }
}

#[cfg(test)]
mod limitations_regression_tests {
    use std::sync::Arc;

    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};

    use super::*;
    use crate::clipboard::UnsupportedClipboard;

    fn app_with_question(multi_select: bool) -> AppState {
        let directory = tempfile::tempdir().expect("tempdir");
        let mut app = AppState::new(
            directory.path().to_path_buf(),
            "limitations-regression",
            "",
            Arc::new(UnsupportedClipboard),
        )
        .expect("app");
        app.open_question(vec![QuestionPrompt {
            header: "Scope".to_owned(),
            question: "What should Medusa do?".to_owned(),
            options: if multi_select {
                vec![QuestionOption {
                    label: "Fix it".to_owned(),
                    description: "Apply the repair".to_owned(),
                }]
            } else {
                Vec::new()
            },
            multi_select,
        }]);
        app
    }

    #[test]
    fn space_is_inserted_in_free_text_question_answers() {
        let mut app = app_with_question(false);
        for code in [KeyCode::Char('a'), KeyCode::Char(' '), KeyCode::Char('b')] {
            app.handle_question_modal_event(Event::Key(KeyEvent::new(code, KeyModifiers::NONE)))
                .expect("type answer");
        }
        assert_eq!(
            app.question_modal
                .as_ref()
                .expect("question")
                .active_custom_answer(),
            "a b"
        );
    }

    #[test]
    fn space_toggles_explicit_multi_select_options() {
        let mut app = app_with_question(true);
        app.handle_question_modal_event(Event::Key(KeyEvent::new(
            KeyCode::Char(' '),
            KeyModifiers::NONE,
        )))
        .expect("toggle option");
        assert_eq!(
            app.question_modal.as_ref().expect("question").answer_for(0),
            Some("Fix it".to_owned())
        );
    }

    #[test]
    fn final_question_submits_without_review_confirmation() {
        let mut app = app_with_question(false);
        app.handle_question_modal_event(Event::Paste("complete all fixes".to_owned()))
            .expect("paste answer");
        let action = app
            .handle_question_modal_event(Event::Key(KeyEvent::new(
                KeyCode::Enter,
                KeyModifiers::NONE,
            )))
            .expect("submit answer");
        assert!(matches!(action, AppAction::AnswerQuestion(_)));
        assert!(app.question_modal.is_none());
    }
}
