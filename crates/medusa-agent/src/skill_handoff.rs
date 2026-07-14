use medusa_skills::SkillIndex;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct HandoffQueue {
    pub pending: Vec<String>,
}

impl HandoffQueue {
    pub fn push(&mut self, skill: impl Into<String>) {
        self.pending.push(skill.into());
    }

    pub fn pop(&mut self) -> Option<String> {
        if self.pending.is_empty() {
            return None;
        }
        Some(self.pending.remove(0))
    }

    pub fn drain(&mut self, index: &SkillIndex) -> HandoffOutcome {
        let mut resolved = Vec::new();
        while let Some(name) = self.pop() {
            if index.by_name(&name).is_some() {
                resolved.push(name);
            }
        }
        HandoffOutcome { resolved }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct HandoffOutcome {
    pub resolved: Vec<String>,
}