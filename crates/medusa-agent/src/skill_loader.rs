use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use medusa_skills::SkillIndex;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LoadedEntry {
    pub skill: medusa_skills::SkillEntry,
    pub depth: usize,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SkillBundle {
    pub entries: Vec<LoadedEntry>,
}

pub fn load(index: &SkillIndex, root: &str, max_depth: usize) -> MedusaResult<SkillBundle> {
    let mut visited: Vec<String> = Vec::new();
    let mut bundle = SkillBundle::default();
    visit(index, root, 0, &mut visited, &mut bundle, max_depth)?;
    Ok(bundle)
}

fn visit(
    index: &SkillIndex,
    name: &str,
    depth: usize,
    visited: &mut Vec<String>,
    bundle: &mut SkillBundle,
    max_depth: usize,
) -> MedusaResult<()> {
    if visited.iter().any(|n| n == name) {
        return Err(MedusaError::new(
            ErrorCode::PolicyDenied,
            ErrorCategory::Policy,
            format!("skill cycle detected: {name} (visited {visited:?})"),
        ));
    }
    if depth >= max_depth {
        return Err(MedusaError::new(
            ErrorCode::PolicyDenied,
            ErrorCategory::Policy,
            format!("skill chain depth {depth} exceeds cap {max_depth}"),
        ));
    }
    let entry = index.by_name(name).ok_or_else(|| {
        MedusaError::new(
            ErrorCode::InvalidConfiguration,
            ErrorCategory::Validation,
            format!("required skill '{name}' not found"),
        )
    })?;
    bundle.entries.push(LoadedEntry {
        skill: entry.clone(),
        depth,
    });
    visited.push(name.to_owned());
    for required in &entry.requires {
        visit(index, required, depth + 1, visited, bundle, max_depth)?;
    }
    visited.pop();
    Ok(())
}