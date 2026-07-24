/// Always-on minimal implementation policy inspired by Ponytail's decision ladder.
use medusa_config::Mode;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CodingPolicyLevel {
    Off,
    Lite,
    Full,
    Ultra,
}

impl CodingPolicyLevel {
    fn from_str(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "off" => Some(Self::Off),
            "lite" => Some(Self::Lite),
            "full" => Some(Self::Full),
            "ultra" => Some(Self::Ultra),
            _ => None,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Lite => "lite",
            Self::Full => "full",
            Self::Ultra => "ultra",
        }
    }
}

fn active_level() -> CodingPolicyLevel {
    std::env::var("MEDUSA_CODING_POLICY")
        .ok()
        .and_then(|value| CodingPolicyLevel::from_str(&value))
        .unwrap_or(CodingPolicyLevel::Full)
}

pub(crate) fn apply(mut prompt: String, mode: Mode) -> String {
    if mode == Mode::ReadOnly {
        return prompt;
    }
    let level = active_level();
    let fragment = prompt_fragment_for(level);
    if !fragment.is_empty() {
        prompt.push_str("\n\n");
        prompt.push_str(&fragment);
    }
    prompt
}

fn prompt_fragment_for(level: CodingPolicyLevel) -> String {
    if level == CodingPolicyLevel::Off {
        return String::new();
    }

    let intensity = match level {
        CodingPolicyLevel::Lite => "Build what was requested, but mention a materially simpler alternative in one short line when one exists.",
        CodingPolicyLevel::Full => "Enforce the ladder. Prefer the shortest correct diff and the fewest touched files.",
        CodingPolicyLevel::Ultra => "Apply strict YAGNI. Prefer deletion over addition and challenge speculative requirements while still shipping the smallest useful result.",
        CodingPolicyLevel::Off => unreachable!(),
    };

    format!(
        "MINIMAL CODING POLICY — ACTIVE ({})\n\n\
This policy governs implementation choices, not requested explanations. {intensity}\n\n\
Before writing or changing code, understand the affected flow and stop at the first applicable option:\n\
1. Do not implement speculative or unnecessary functionality.\n\
2. Reuse an existing repository helper, type, component, command, or pattern.\n\
3. Prefer the language standard library.\n\
4. Prefer a native platform, browser, operating-system, database, or framework feature.\n\
5. Reuse an already-installed dependency.\n\
6. Use a direct expression when it remains clear and correct.\n\
7. Otherwise implement the smallest complete solution.\n\n\
Inspect before choosing. Trace the real flow and relevant callers. For bug fixes, correct the shared root cause once instead of adding repeated symptom guards.\n\n\
Do not introduce unrequested abstractions, speculative scaffolding, wrappers with one implementation, configuration for constants, or avoidable dependencies. Prefer consolidation and deletion over addition. Touch the fewest files needed for a correct change.\n\n\
Minimal does not mean negligent. Preserve security, trust-boundary validation, accessibility, data integrity, error handling that prevents loss, concurrency correctness, compatibility requirements, and anything explicitly requested.\n\n\
Never weaken, delete, or rewrite tests merely to make a failing implementation pass. Modify tests only when behavior intentionally changes or new behavior requires coverage. Fix product code rather than expected outputs when the test captures the intended contract. Always run the smallest relevant verification.\n\n\
A new dependency requires an explicit internal check that the standard library, native platform, existing dependency set, and a small local implementation are insufficient.",
        level.as_str()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_policy_contains_the_ladder_and_safety_boundaries() {
        let prompt = prompt_fragment_for(CodingPolicyLevel::Full);
        assert!(prompt.contains("MINIMAL CODING POLICY — ACTIVE (full)"));
        assert!(prompt.contains("Reuse an existing repository helper"));
        assert!(prompt.contains("Never weaken, delete, or rewrite tests"));
    }

    #[test]
    fn off_removes_the_policy_fragment() {
        assert!(prompt_fragment_for(CodingPolicyLevel::Off).is_empty());
    }

    #[test]
    fn invalid_levels_are_rejected() {
        assert_eq!(CodingPolicyLevel::from_str("maximum"), None);
    }
}
