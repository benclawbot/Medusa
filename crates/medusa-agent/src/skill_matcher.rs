use medusa_config::SkillConfig;
use medusa_core::MedusaResult;
use medusa_skills::{SkillEntry, SkillIndex};

#[derive(Clone, Debug, PartialEq)]
pub struct SkillMatch {
    pub skill: SkillEntry,
    pub score: f32,
    pub matched_triggers: Vec<String>,
}

pub fn match_prompt(
    prompt: &str,
    index: &SkillIndex,
    config: &SkillConfig,
) -> MedusaResult<Vec<SkillMatch>> {
    if !config.enabled {
        return Ok(Vec::new());
    }
    let prompt_lower = prompt.to_ascii_lowercase();
    let mut results: Vec<SkillMatch> = index
        .entries()
        .iter()
        .map(|entry| score_entry(entry, &prompt_lower))
        .filter(|m| !m.matched_triggers.is_empty())
        .collect();
    results.sort_by(|left, right| {
        right
            .score
            .partial_cmp(&left.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| left.skill.name.cmp(&right.skill.name))
    });
    results.truncate(config.max_matches);
    Ok(results)
}

fn score_entry(entry: &SkillEntry, prompt_lower: &str) -> SkillMatch {
    let matched: Vec<String> = entry
        .manifest
        .triggers
        .iter()
        .filter(|trigger| prompt_lower.contains(&trigger.to_ascii_lowercase()))
        .cloned()
        .collect();
    SkillMatch {
        skill: entry.clone(),
        score: matched.len() as f32,
        matched_triggers: matched,
    }
}
