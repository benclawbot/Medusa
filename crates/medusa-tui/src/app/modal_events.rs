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
                        let configuration = self
                            .model_modal
                            .take()
                            .expect("model modal exists")
                            .configuration();
                        Ok(AppAction::ConfigureModel(configuration))
                    } else {
                        self.model_modal
                            .as_mut()
                            .expect("model modal exists")
                            .cycle_focus();
                        Ok(AppAction::Redraw)
                    }
                }
                KeyCode::BackTab => {
                    self.model_modal
                        .as_mut()
                        .expect("model modal exists")
                        .cycle_focus_back();
                    Ok(AppAction::Redraw)
                }
                KeyCode::Tab if key.modifiers.contains(KeyModifiers::SHIFT) => {
                    self.model_modal
                        .as_mut()
                        .expect("model modal exists")
                        .cycle_focus_back();
                    Ok(AppAction::Redraw)
                }
                KeyCode::Tab => {
                    self.model_modal
                        .as_mut()
                        .expect("model modal exists")
                        .cycle_focus();
                    Ok(AppAction::Redraw)
                }
                KeyCode::Up | KeyCode::Left => {
                    self.model_modal
                        .as_mut()
                        .expect("model modal exists")
                        .move_selection(-1);
                    Ok(AppAction::Redraw)
                }
                KeyCode::Down | KeyCode::Right => {
                    self.model_modal
                        .as_mut()
                        .expect("model modal exists")
                        .move_selection(1);
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
                self.question_modal
                    .as_mut()
                    .expect("question modal exists")
                    .insert_answer(&text);
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
                    self.question_modal
                        .as_mut()
                        .expect("question modal exists")
                        .move_question(-1);
                    Ok(AppAction::Redraw)
                }
                KeyCode::Tab if key.modifiers.contains(KeyModifiers::SHIFT) => {
                    self.question_modal
                        .as_mut()
                        .expect("question modal exists")
                        .move_question(-1);
                    Ok(AppAction::Redraw)
                }
                KeyCode::Tab => {
                    self.question_modal
                        .as_mut()
                        .expect("question modal exists")
                        .move_question(1);
                    Ok(AppAction::Redraw)
                }
                KeyCode::Up | KeyCode::Left => {
                    if self
                        .question_modal
                        .as_ref()
                        .is_some_and(QuestionModal::is_reviewing)
                    {
                        return Ok(AppAction::Redraw);
                    }
                    self.question_modal
                        .as_mut()
                        .expect("question modal exists")
                        .move_selection(-1);
                    Ok(AppAction::Redraw)
                }
                KeyCode::Down | KeyCode::Right => {
                    if self
                        .question_modal
                        .as_ref()
                        .is_some_and(QuestionModal::is_reviewing)
                    {
                        return Ok(AppAction::Redraw);
                    }
                    self.question_modal
                        .as_mut()
                        .expect("question modal exists")
                        .move_selection(1);
                    Ok(AppAction::Redraw)
                }
                KeyCode::Backspace => {
                    if self
                        .question_modal
                        .as_ref()
                        .is_some_and(|modal| !modal.is_reviewing())
                    {
                        self.question_modal
                            .as_mut()
                            .expect("question modal exists")
                            .delete_answer_character();
                    }
                    Ok(AppAction::Redraw)
                }
                KeyCode::Char('v') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    if self
                        .question_modal
                        .as_ref()
                        .is_some_and(|modal| !modal.is_reviewing())
                        && let ClipboardContent::Text(text) = self.clipboard.read()?
                    {
                        self.question_modal
                            .as_mut()
                            .expect("question modal exists")
                            .insert_answer(&text);
                    }
                    Ok(AppAction::Redraw)
                }
                KeyCode::Char(' ') if key.modifiers.is_empty() => {
                    if self
                        .question_modal
                        .as_ref()
                        .is_some_and(|modal| !modal.is_reviewing())
                    {
                        self.question_modal
                            .as_mut()
                            .expect("question modal exists")
                            .toggle_current_option();
                    }
                    Ok(AppAction::Redraw)
                }
                KeyCode::Char(character)
                    if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT =>
                {
                    if self
                        .question_modal
                        .as_ref()
                        .is_some_and(|modal| !modal.is_reviewing())
                    {
                        self.question_modal
                            .as_mut()
                            .expect("question modal exists")
                            .insert_answer(&character.to_string());
                    }
                    Ok(AppAction::Redraw)
                }
                KeyCode::Enter => {
                    let modal = self.question_modal.as_mut().expect("question modal exists");
                    if modal.is_reviewing() {
                        let Some(answer) = modal.answer_bundle() else {
                            self.status = "answer every question before confirming".to_owned();
                            return Ok(AppAction::Redraw);
                        };
                        self.question_modal = None;
                        self.transcript.push(TranscriptEntry::User(PromptDraft {
                            text: answer.clone(),
                            ..PromptDraft::default()
                        }));
                        self.status = "answers confirmed".to_owned();
                        return Ok(AppAction::AnswerQuestion(answer));
                    }
                    if !modal.select_current_answer() {
                        self.status = "choose or type an answer".to_owned();
                        return Ok(AppAction::Redraw);
                    }
                    modal.advance_or_review();
                    Ok(AppAction::Redraw)
                }
                _ => Ok(AppAction::None),
            },
            _ => Ok(AppAction::None),
        }
    }
}
