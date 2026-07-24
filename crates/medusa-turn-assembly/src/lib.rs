//! Deterministic model-turn assembly with stable prefixes and hard budgets.

use std::collections::BTreeSet;

use medusa_context::ContextItem;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TurnBudget {
    pub maximum_input_tokens: usize,
    pub reserved_output_tokens: usize,
}

impl TurnBudget {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.maximum_input_tokens == 0 {
            return Err("maximum_input_tokens must be greater than zero");
        }
        if self.reserved_output_tokens >= self.maximum_input_tokens {
            return Err("reserved_output_tokens must be smaller than maximum_input_tokens");
        }
        Ok(())
    }

    #[must_use]
    pub fn available_input_tokens(&self) -> usize {
        self.maximum_input_tokens - self.reserved_output_tokens
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct StableSection {
    pub name: String,
    pub content: String,
}

impl StableSection {
    pub fn new(name: impl Into<String>, content: impl Into<String>) -> Result<Self, &'static str> {
        let name = name.into();
        let content = content.into();
        if name.trim().is_empty() {
            return Err("stable section name cannot be empty");
        }
        if content.trim().is_empty() {
            return Err("stable section content cannot be empty");
        }
        Ok(Self { name, content })
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ToolSchema {
    pub name: String,
    pub canonical_json: String,
}

impl ToolSchema {
    pub fn new(name: impl Into<String>, canonical_json: impl Into<String>) -> Result<Self, &'static str> {
        let name = name.into();
        let canonical_json = canonical_json.into();
        if name.trim().is_empty() {
            return Err("tool schema name cannot be empty");
        }
        if canonical_json.trim().is_empty() {
            return Err("tool schema content cannot be empty");
        }
        Ok(Self { name, canonical_json })
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TurnAssemblyInput {
    pub stable_sections: Vec<StableSection>,
    pub tool_schemas: Vec<ToolSchema>,
    pub retrieved_context: Vec<ContextItem>,
    pub current_task: String,
    pub budget: TurnBudget,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AssemblySection {
    pub name: String,
    pub stable: bool,
    pub content: String,
    pub estimated_tokens: usize,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TurnAssembly {
    pub sections: Vec<AssemblySection>,
    pub included_context_ids: Vec<String>,
    pub omitted_context_ids: Vec<String>,
    pub stable_prefix_fingerprint: String,
    pub full_prompt_fingerprint: String,
    pub estimated_input_tokens: usize,
    pub available_input_tokens: usize,
    pub rendered_prompt: String,
}

impl TurnAssemblyInput {
    pub fn assemble(mut self) -> Result<TurnAssembly, &'static str> {
        self.budget.validate()?;
        if self.current_task.trim().is_empty() {
            return Err("current task cannot be empty");
        }
        validate_unique_names(&self.stable_sections, &self.tool_schemas)?;
        validate_context(&self.retrieved_context)?;

        self.tool_schemas.sort_by(|left, right| left.name.cmp(&right.name));
        self.retrieved_context.sort_by_key(|item| item.sequence);

        let mut sections = Vec::new();
        for section in self.stable_sections {
            sections.push(section_from(section.name, true, section.content));
        }
        for tool in self.tool_schemas {
            sections.push(section_from(
                format!("tool:{}", tool.name),
                true,
                tool.canonical_json,
            ));
        }

        let stable_tokens: usize = sections.iter().map(|section| section.estimated_tokens).sum();
        let task = section_from("current_task".to_owned(), false, self.current_task);
        let required_tokens = stable_tokens.saturating_add(task.estimated_tokens);
        let available = self.budget.available_input_tokens();
        if required_tokens > available {
            return Err("stable prefix and current task exceed the input budget");
        }

        let mut used = required_tokens;
        let mut included_context_ids = Vec::new();
        let mut omitted_context_ids = Vec::new();
        for item in self.retrieved_context {
            let content = render_context_item(&item);
            let candidate = section_from(format!("context:{}", item.id), false, content);
            if used.saturating_add(candidate.estimated_tokens) <= available {
                used += candidate.estimated_tokens;
                included_context_ids.push(item.id);
                sections.push(candidate);
            } else if item.terminal || is_execution_critical(&item) {
                return Err("execution-critical retrieved context does not fit the input budget");
            } else {
                omitted_context_ids.push(item.id);
            }
        }
        sections.push(task);

        let stable_rendered = render_sections(sections.iter().filter(|section| section.stable));
        let rendered_prompt = render_sections(sections.iter());
        let stable_prefix_fingerprint = fingerprint(stable_rendered.as_bytes());
        let full_prompt_fingerprint = fingerprint(rendered_prompt.as_bytes());

        Ok(TurnAssembly {
            sections,
            included_context_ids,
            omitted_context_ids,
            stable_prefix_fingerprint,
            full_prompt_fingerprint,
            estimated_input_tokens: used,
            available_input_tokens: available,
            rendered_prompt,
        })
    }
}

fn section_from(name: String, stable: bool, content: String) -> AssemblySection {
    let estimated_tokens = estimate_tokens(&content);
    AssemblySection {
        name,
        stable,
        content,
        estimated_tokens,
    }
}

fn validate_unique_names(stable: &[StableSection], tools: &[ToolSchema]) -> Result<(), &'static str> {
    let mut names = BTreeSet::new();
    for name in stable.iter().map(|item| item.name.as_str()).chain(tools.iter().map(|item| item.name.as_str())) {
        if !names.insert(name) {
            return Err("stable section and tool names must be unique");
        }
    }
    Ok(())
}

fn validate_context(items: &[ContextItem]) -> Result<(), &'static str> {
    let mut ids = BTreeSet::new();
    let mut previous_sequence = 0;
    for item in items {
        if !ids.insert(item.id.as_str()) {
            return Err("retrieved context contains a duplicate id");
        }
        if item.sequence <= previous_sequence {
            return Err("retrieved context must be supplied in increasing source sequence");
        }
        previous_sequence = item.sequence;
    }
    Ok(())
}

fn render_context_item(item: &ContextItem) -> String {
    format!(
        "id={}\nkind={:?}\nterminal={}\ncontent={}",
        item.id, item.kind, item.terminal, item.content
    )
}

fn render_sections<'a>(sections: impl Iterator<Item = &'a AssemblySection>) -> String {
    let mut rendered = String::new();
    for section in sections {
        rendered.push_str("<section name=\"");
        rendered.push_str(&section.name);
        rendered.push_str("\">\n");
        rendered.push_str(&section.content);
        rendered.push_str("\n</section>\n");
    }
    rendered
}

fn is_execution_critical(item: &ContextItem) -> bool {
    use medusa_context::ContextKind;
    matches!(
        item.kind,
        ContextKind::Goal
            | ContextKind::Constraint
            | ContextKind::Todo
            | ContextKind::Blocker
            | ContextKind::Failure
            | ContextKind::Evidence
            | ContextKind::Checkpoint
    )
}

#[must_use]
pub fn estimate_tokens(content: &str) -> usize {
    content.len().saturating_add(3) / 4
}

fn fingerprint(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use medusa_context::{ContextItem, ContextKind};
    use time::macros::datetime;

    fn context(id: &str, kind: ContextKind, sequence: u64, content: &str) -> ContextItem {
        ContextItem::new(id, kind, content, sequence, datetime!(2026-07-24 14:00 UTC)).expect("context")
    }

    fn input(task: &str) -> TurnAssemblyInput {
        TurnAssemblyInput {
            stable_sections: vec![StableSection::new("system", "Follow policy").expect("stable")],
            tool_schemas: vec![ToolSchema::new("read", "{\"type\":\"object\"}").expect("tool")],
            retrieved_context: vec![context("goal", ContextKind::Goal, 1, "Finish the task")],
            current_task: task.to_owned(),
            budget: TurnBudget { maximum_input_tokens: 512, reserved_output_tokens: 128 },
        }
    }

    #[test]
    fn dynamic_task_does_not_change_stable_prefix() {
        let first = input("Implement A").assemble().expect("first");
        let second = input("Implement B").assemble().expect("second");
        assert_eq!(first.stable_prefix_fingerprint, second.stable_prefix_fingerprint);
        assert_ne!(first.full_prompt_fingerprint, second.full_prompt_fingerprint);
    }

    #[test]
    fn tool_order_is_canonical() {
        let mut value = input("task");
        value.tool_schemas = vec![
            ToolSchema::new("z", "{}").expect("z"),
            ToolSchema::new("a", "{}").expect("a"),
        ];
        let assembled = value.assemble().expect("assembly");
        let names: Vec<_> = assembled.sections.iter().filter(|item| item.stable).map(|item| item.name.as_str()).collect();
        assert_eq!(names, vec!["system", "tool:a", "tool:z"]);
    }

    #[test]
    fn optional_context_is_omitted_under_budget_pressure() {
        let mut value = input("task");
        value.retrieved_context.push(context("observation", ContextKind::Observation, 2, &"x".repeat(800)));
        let assembled = value.assemble().expect("assembly");
        assert_eq!(assembled.omitted_context_ids, vec!["observation"]);
    }

    #[test]
    fn critical_context_never_silently_drops() {
        let mut value = input("task");
        value.retrieved_context.push(context("blocker", ContextKind::Blocker, 2, &"x".repeat(800)));
        assert_eq!(value.assemble(), Err("execution-critical retrieved context does not fit the input budget"));
    }

    #[test]
    fn duplicate_names_are_rejected() {
        let mut value = input("task");
        value.tool_schemas = vec![ToolSchema::new("system", "{}").expect("tool")];
        assert_eq!(value.assemble(), Err("stable section and tool names must be unique"));
    }
}
