use crate::{
    clipboard::PromptDraft,
    commands::{Effort, ModelConfiguration, SlashCommand},
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TranscriptEntry {
    User(PromptDraft),
    Assistant(String),
    Activity(TranscriptActivity),
    System(String),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TranscriptActivityKind {
    Assistant,
    Done,
    Error,
    Progress,
    Tool,
    Verification,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TranscriptActivity {
    pub id: Option<String>,
    pub kind: TranscriptActivityKind,
    pub title: String,
    pub details: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TranscriptPlan {
    pub steps: Vec<TranscriptPlanStep>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TranscriptPlanStep {
    pub title: String,
    pub state: TranscriptPlanStepState,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TranscriptPlanStepState {
    Pending,
    Active,
    Completed,
    Failed,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AppAction {
    None,
    Redraw,
    Submit(PromptDraft),
    AnswerQuestion(String),
    Command(SlashCommand),
    ConfigureModel(ModelConfiguration),
    Interrupt,
    Quit,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QuestionOption {
    pub label: String,
    pub description: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QuestionPrompt {
    pub header: String,
    pub question: String,
    pub options: Vec<QuestionOption>,
    pub multi_select: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Default)]
struct QuestionAnswer {
    selected_options: Vec<usize>,
    custom_answer: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QuestionModal {
    questions: Vec<QuestionPrompt>,
    answers: Vec<QuestionAnswer>,
    selected_options: Vec<usize>,
    active_question: usize,
    reviewing: bool,
}

impl QuestionModal {
    pub fn new(questions: Vec<QuestionPrompt>) -> Self {
        let count = questions.len();
        Self {
            questions,
            answers: vec![QuestionAnswer::default(); count],
            selected_options: vec![0; count],
            active_question: 0,
            reviewing: false,
        }
    }

    #[must_use]
    pub fn questions(&self) -> &[QuestionPrompt] {
        &self.questions
    }

    #[must_use]
    pub fn active_question(&self) -> usize {
        self.active_question
    }

    #[must_use]
    pub fn is_reviewing(&self) -> bool {
        self.reviewing
    }

    #[must_use]
    pub fn active_prompt(&self) -> Option<&QuestionPrompt> {
        self.questions.get(self.active_question)
    }

    #[must_use]
    pub fn active_selected_option(&self) -> usize {
        self.selected_options
            .get(self.active_question)
            .copied()
            .unwrap_or_default()
    }

    #[must_use]
    pub fn active_custom_answer(&self) -> &str {
        self.answers
            .get(self.active_question)
            .map(|answer| answer.custom_answer.as_str())
            .unwrap_or_default()
    }

    pub(super) fn move_selection(&mut self, delta: isize) {
        let active_question = self.active_question;
        let option_count = self
            .questions
            .get(active_question)
            .map_or(0, |prompt| prompt.options.len());
        if let Some(selected) = self.selected_options.get_mut(active_question)
            && option_count > 0
        {
            *selected = cycle_index(*selected, option_count, delta);
        }
    }

    pub(super) fn move_question(&mut self, delta: isize) {
        if self.questions.is_empty() {
            return;
        }
        if self.reviewing {
            self.reviewing = false;
            self.active_question = self.questions.len().saturating_sub(1);
            return;
        }
        self.active_question = cycle_index(self.active_question, self.questions.len(), delta);
    }

    pub(super) fn advance_or_review(&mut self) {
        if self.active_question.saturating_add(1) < self.questions.len() {
            self.active_question = self.active_question.saturating_add(1);
        } else {
            self.reviewing = true;
        }
    }

    pub(super) fn toggle_current_option(&mut self) {
        let active_question = self.active_question;
        let Some((option_count, multi_select)) = self
            .questions
            .get(active_question)
            .map(|prompt| (prompt.options.len(), prompt.multi_select))
        else {
            return;
        };
        if option_count == 0 || !multi_select {
            return;
        }
        let selected = self.active_selected_option();
        let answer = &mut self.answers[active_question];
        if let Some(position) = answer
            .selected_options
            .iter()
            .position(|option| *option == selected)
        {
            answer.selected_options.remove(position);
        } else {
            answer.selected_options.push(selected);
            answer.selected_options.sort_unstable();
        }
    }

    pub(super) fn select_current_answer(&mut self) -> bool {
        let active_question = self.active_question;
        let Some((option_count, multi_select)) = self
            .questions
            .get(active_question)
            .map(|prompt| (prompt.options.len(), prompt.multi_select))
        else {
            return false;
        };
        if option_count > 0 && !multi_select {
            let selected = self.active_selected_option();
            self.answers[active_question].selected_options = vec![selected];
        } else if option_count > 0 && self.answer_for(active_question).is_none() {
            self.toggle_current_option();
        }
        self.answer_for(active_question).is_some()
    }

    pub(super) fn insert_answer(&mut self, text: &str) {
        if let Some(answer) = self.answers.get_mut(self.active_question) {
            answer.custom_answer.push_str(text);
        }
    }

    pub(super) fn delete_answer_character(&mut self) {
        if let Some(answer) = self.answers.get_mut(self.active_question) {
            answer.custom_answer.pop();
        }
    }

    #[must_use]
    pub fn answer_for(&self, index: usize) -> Option<String> {
        let prompt = self.questions.get(index)?;
        let answer = self.answers.get(index)?;
        (!answer.custom_answer.trim().is_empty())
            .then(|| answer.custom_answer.trim().to_owned())
            .or_else(|| {
                let labels = answer
                    .selected_options
                    .iter()
                    .filter_map(|option| prompt.options.get(*option))
                    .map(|option| option.label.as_str())
                    .collect::<Vec<_>>();
                (!labels.is_empty()).then(|| labels.join(", "))
            })
    }

    pub(super) fn answer_bundle(&self) -> Option<String> {
        let answers = self
            .questions
            .iter()
            .enumerate()
            .map(|(index, prompt)| {
                self.answer_for(index)
                    .map(|answer| format!("{}: {answer}", prompt.header))
            })
            .collect::<Option<Vec<_>>>()?;
        Some(answers.join("\n"))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ModelModalFocus {
    Provider,
    Model,
    Effort,
    ApiKey,
    Apply,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelModal {
    provider_index: usize,
    model_index: usize,
    model_options: Vec<String>,
    effort: Effort,
    focus: ModelModalFocus,
    api_key: String,
    has_existing_key: bool,
}

impl ModelModal {
    const PROVIDERS: [&str; 3] = ["minimax", "anthropic", "anthropic-compatible"];

    pub(super) fn new(
        model_label: Option<&str>,
        effort_label: Option<&str>,
        has_existing_key: bool,
    ) -> Self {
        let (provider, current_model) = model_label
            .and_then(|label| label.split_once(" / "))
            .unwrap_or(("minimax", "MiniMax-M3"));
        let provider_index = Self::PROVIDERS
            .iter()
            .position(|candidate| *candidate == provider)
            .unwrap_or(0);
        let models = model_options_for(Self::PROVIDERS[provider_index], current_model);
        let model_index = models
            .iter()
            .position(|candidate| candidate == current_model)
            .unwrap_or(0);
        Self {
            provider_index,
            model_index,
            model_options: models,
            effort: effort_from_label(effort_label),
            focus: ModelModalFocus::Model,
            api_key: String::new(),
            has_existing_key,
        }
    }

    #[must_use]
    pub fn provider(&self) -> &str {
        Self::PROVIDERS[self.provider_index]
    }

    #[must_use]
    pub fn model_options(&self) -> Vec<String> {
        self.model_options.clone()
    }

    #[must_use]
    pub fn selected_model(&self) -> String {
        self.model_options
            .get(self.model_index)
            .cloned()
            .unwrap_or_else(|| "MiniMax-M3".to_owned())
    }

    #[must_use]
    pub const fn selected_model_index(&self) -> usize {
        self.model_index
    }

    #[must_use]
    pub const fn effort(&self) -> Effort {
        self.effort
    }

    #[must_use]
    pub const fn focus(&self) -> ModelModalFocus {
        self.focus
    }

    #[must_use]
    pub fn api_key_mask(&self) -> String {
        if self.api_key.is_empty() {
            if self.has_existing_key {
                "configured".to_owned()
            } else {
                "not configured".to_owned()
            }
        } else {
            "*".repeat(self.api_key.chars().count())
        }
    }

    #[must_use]
    pub fn configuration(&self) -> ModelConfiguration {
        ModelConfiguration {
            provider: self.provider().to_owned(),
            model: self.selected_model(),
            effort: self.effort,
            api_key: (!self.api_key.is_empty()).then(|| self.api_key.clone()),
        }
    }

    pub(super) fn cycle_focus(&mut self) {
        self.focus = match self.focus {
            ModelModalFocus::Provider => ModelModalFocus::Model,
            ModelModalFocus::Model => ModelModalFocus::Effort,
            ModelModalFocus::Effort => ModelModalFocus::ApiKey,
            ModelModalFocus::ApiKey => ModelModalFocus::Apply,
            ModelModalFocus::Apply => ModelModalFocus::Provider,
        };
    }

    pub(super) fn cycle_focus_back(&mut self) {
        self.focus = match self.focus {
            ModelModalFocus::Provider => ModelModalFocus::Apply,
            ModelModalFocus::Model => ModelModalFocus::Provider,
            ModelModalFocus::Effort => ModelModalFocus::Model,
            ModelModalFocus::ApiKey => ModelModalFocus::Effort,
            ModelModalFocus::Apply => ModelModalFocus::ApiKey,
        };
    }

    pub(super) fn focus_api_key(&mut self) {
        self.focus = ModelModalFocus::ApiKey;
    }

    pub(super) fn move_selection(&mut self, delta: isize) {
        match self.focus {
            ModelModalFocus::Provider => {
                self.provider_index =
                    cycle_index(self.provider_index, Self::PROVIDERS.len(), delta);
                self.model_options = model_options_for(self.provider(), "");
                self.model_index = 0;
            }
            ModelModalFocus::Model => {
                self.model_index = cycle_index(self.model_index, self.model_options.len(), delta);
            }
            ModelModalFocus::Effort => {
                const EFFORTS: [Effort; 4] =
                    [Effort::Low, Effort::Medium, Effort::High, Effort::Auto];
                let index = EFFORTS
                    .iter()
                    .position(|candidate| *candidate == self.effort)
                    .unwrap_or(2);
                self.effort = EFFORTS[cycle_index(index, EFFORTS.len(), delta)];
            }
            ModelModalFocus::ApiKey | ModelModalFocus::Apply => {}
        }
    }

    pub(super) fn insert_key_text(&mut self, text: &str) {
        self.api_key
            .extend(text.chars().filter(|character| !character.is_whitespace()));
    }

    pub(super) fn delete_key_character(&mut self) {
        self.api_key.pop();
    }
}

fn model_options_for(provider: &str, current_model: &str) -> Vec<String> {
    let mut models = match provider {
        "minimax" => vec!["MiniMax-M3".to_owned(), "MiniMax-M2.7".to_owned()],
        "anthropic" => vec![
            "claude-opus-4-6".to_owned(),
            "claude-sonnet-4-6".to_owned(),
            "claude-haiku-4-5".to_owned(),
        ],
        _ => vec!["custom-model".to_owned()],
    };
    if !current_model.is_empty() && !models.iter().any(|model| model == current_model) {
        models.insert(0, current_model.to_owned());
    }
    models
}

fn effort_from_label(label: Option<&str>) -> Effort {
    match label.unwrap_or_default() {
        "effort:low" => Effort::Low,
        "effort:medium" => Effort::Medium,
        "effort:auto" => Effort::Auto,
        _ => Effort::High,
    }
}

fn cycle_index(current: usize, length: usize, delta: isize) -> usize {
    (current as isize + delta).rem_euclid(length as isize) as usize
}
