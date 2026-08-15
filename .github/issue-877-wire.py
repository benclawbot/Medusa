from pathlib import Path

root = Path('crates/medusa-session-continuity/src/root.rs')
text = root.read_text()
text = text.replace('const MAX_TRAJECTORY_TEXT_BYTES: usize = 16 * 1024;', 'const MAX_TRAJECTORY_TEXT_BYTES: usize = 32 * 1024;')
anchor = '''#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DisprovedHypothesisCheckpoint {
    pub signature: String,
    pub repository_fingerprint: String,
}
'''
insert = anchor + '''
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoadblockClass {
    DeterministicFailure,
    MissingCapability,
    DependencyUnavailable,
    PermissionPolicy,
    ArchitectureCompatibility,
    RepositoryConflict,
    DisprovedHypothesis,
    StructuralVerification,
    ResourceExhaustion,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoadblockDisposition {
    AlternativeSelected,
    EscalationRequired,
    Resolved,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AlternativePathCheckpoint {
    pub strategy: String,
    pub rationale: String,
    pub success_probability: u8,
    pub blast_radius: u8,
    pub verifiability: u8,
    pub reversibility: u8,
    #[serde(default)]
    pub evidence_refs: Vec<String>,
    #[serde(default)]
    pub verification_requirements: Vec<String>,
    #[serde(default)]
    pub selected: bool,
    #[serde(default)]
    pub rejected_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoadblockCheckpoint {
    pub fingerprint: String,
    pub class: RoadblockClass,
    pub summary: String,
    pub first_generation: u64,
    pub last_generation: u64,
    pub repository_fingerprint: String,
    pub abandoned_strategy: String,
    pub selected_alternative: Option<String>,
    #[serde(default)]
    pub alternatives: Vec<AlternativePathCheckpoint>,
    #[serde(default)]
    pub source_refs: Vec<String>,
    pub disposition: RoadblockDisposition,
}

impl RoadblockCheckpoint {
    pub fn unresolved(&self) -> bool {
        !matches!(self.disposition, RoadblockDisposition::Resolved)
    }
}
'''
assert text.count(anchor) == 1
text = text.replace(anchor, insert)
field_anchor = '''    #[serde(default)]
    pub repair_ledger_cursor: u64,
    pub disproved_hypotheses: Vec<DisprovedHypothesisCheckpoint>,
'''
field_repl = '''    #[serde(default)]
    pub repair_ledger_cursor: u64,
    #[serde(default)]
    pub roadblocks: Vec<RoadblockCheckpoint>,
    #[serde(default)]
    pub strategy_transition_count: u32,
    pub disproved_hypotheses: Vec<DisprovedHypothesisCheckpoint>,
'''
assert text.count(field_anchor) == 1
text = text.replace(field_anchor, field_repl)
default_anchor = '''            repair_ledger: Vec::new(),
            verification_generation: 0,
            repair_ledger_cursor: 0,
            disproved_hypotheses: Vec::new(),
'''
default_repl = '''            repair_ledger: Vec::new(),
            verification_generation: 0,
            repair_ledger_cursor: 0,
            roadblocks: Vec::new(),
            strategy_transition_count: 0,
            disproved_hypotheses: Vec::new(),
'''
assert text.count(default_anchor) == 1
text = text.replace(default_anchor, default_repl)
length_anchor = '''            self.failure_history.len(),
            self.repair_ledger.len(),
            self.disproved_hypotheses.len(),
'''
length_repl = '''            self.failure_history.len(),
            self.repair_ledger.len(),
            self.roadblocks.len(),
            self.disproved_hypotheses.len(),
'''
assert text.count(length_anchor) == 1
text = text.replace(length_anchor, length_repl)
root.write_text(text)

road = Path('crates/medusa-runtime/src/roadblock_recovery.rs')
text = road.read_text()
attempt_anchor = '''    let attempted = prior
        .values()
        .flat_map(|item| item.alternatives.iter())
        .filter(|item| item.selected || item.rejected_reason.is_some())
        .map(|item| strategy_signature(&item.strategy))
        .collect::<BTreeSet<_>>();
'''
attempt_repl = '''    let attempted = trajectory
        .repair_ledger
        .iter()
        .flat_map(|failure| failure.repairs.iter())
        .filter(|attempt| {
            attempt.outcome == medusa_session_continuity::VerificationOutcome::Failed
                && !attempt.hypothesis.trim().is_empty()
        })
        .map(|attempt| strategy_signature(&attempt.hypothesis))
        .collect::<BTreeSet<_>>();
'''
assert text.count(attempt_anchor) == 1
text = text.replace(attempt_anchor, attempt_repl)
selection_anchor = '''        for (index, item) in alternatives.iter_mut().enumerate() {
            item.selected = Some(index) == selected_index;
        }
        let selected_alternative = selected_index.map(|index| alternatives[index].strategy.clone());
'''
selection_repl = '''        let selected_alternative = selected_index.map(|index| alternatives[index].strategy.clone());
        for (index, item) in alternatives.iter_mut().enumerate() {
            item.selected = Some(index) == selected_index;
            if !item.selected && item.rejected_reason.is_none() {
                item.rejected_reason = Some(match selected_alternative.as_deref() {
                    Some(selected) => format!("lower ranked than selected alternative `{selected}`"),
                    None => "strategy transition budget exhausted".to_owned(),
                });
            }
        }
'''
assert text.count(selection_anchor) == 1
text = text.replace(selection_anchor, selection_repl)
structural_anchor = '''    if contains_any(&text, &["assumption", "hypothesis", "does not exist", "no method named", "unresolved import"]) {
        return Some(RoadblockClass::DisprovedHypothesis);
    }
'''
structural_repl = structural_anchor + '''    if contains_any(&text, &["structurally wrong", "invariant violation", "design cannot satisfy", "structural verification"]) {
        return Some(RoadblockClass::StructuralVerification);
    }
'''
assert text.count(structural_anchor) == 1
text = text.replace(structural_anchor, structural_repl)
road.write_text(text)

coding = Path('crates/medusa-runtime/src/coding_trajectory.rs')
text = coding.read_text()
mod_anchor = '''#[path = "repair_ledger.rs"]
mod repair_ledger;
'''
mod_repl = mod_anchor + '''#[path = "roadblock_recovery.rs"]
mod roadblock_recovery;
'''
assert text.count(mod_anchor) == 1
text = text.replace(mod_anchor, mod_repl)
projection_anchor = '''    trajectory.repair_ledger = repair_projection.entries;
    trajectory.verification_generation = repair_projection.generation;
    trajectory.repair_ledger_cursor = repair_projection.cursor;
    trajectory.continuation_intent = session
        .plan
        .iter()
        .find(|step| step.status != AgentPlanStepStatus::Completed)
        .map(|step| format!("continue plan step: {}", step.title));
'''
projection_repl = '''    trajectory.repair_ledger = repair_projection.entries;
    trajectory.verification_generation = repair_projection.generation;
    trajectory.repair_ledger_cursor = repair_projection.cursor;

    let previously_selected = trajectory
        .roadblocks
        .iter()
        .filter(|roadblock| roadblock.unresolved())
        .filter_map(|roadblock| roadblock.selected_alternative.clone())
        .collect::<BTreeSet<_>>();
    let roadblock_projection = roadblock_recovery::project(&trajectory);
    let selected_strategy = roadblock_projection.selected_strategy;
    if selected_strategy
        .as_ref()
        .is_some_and(|strategy| !previously_selected.contains(strategy))
    {
        trajectory.strategy_transition_count = trajectory.strategy_transition_count.saturating_add(1);
    }
    trajectory.roadblocks = roadblock_projection.roadblocks;
    trajectory
        .remaining_blockers
        .retain(|item| !item.starts_with("roadblock:"));
    for roadblock in trajectory.roadblocks.iter().filter(|item| item.unresolved()) {
        trajectory.remaining_blockers.push(format!(
            "roadblock:{:?}:{}",
            roadblock.class, roadblock.summary
        ));
    }
    trajectory.remaining_blockers.sort();
    trajectory.remaining_blockers.dedup();
    trajectory.rejected_alternatives = trajectory
        .roadblocks
        .iter()
        .flat_map(|roadblock| roadblock.alternatives.iter())
        .filter_map(|alternative| {
            alternative.rejected_reason.as_ref().map(|reason| {
                format!("{}: {}", alternative.strategy, reason)
            })
        })
        .take(128)
        .collect();
    trajectory.continuation_intent = selected_strategy
        .map(|strategy| format!("switch strategy: {strategy}"))
        .or_else(|| {
            session
                .plan
                .iter()
                .find(|step| step.status != AgentPlanStepStatus::Completed)
                .map(|step| format!("continue plan step: {}", step.title))
        });
'''
assert text.count(projection_anchor) == 1
text = text.replace(projection_anchor, projection_repl)
render_anchor = '''        "[medusa-coding-trajectory-v1]\\nAuthoritative compact trajectory derived from the canonical journal. Preserve immutable objective/constraints and use repair_ledger as the complete actionable failure set. Repair all independent diagnostics from the latest verification generation together, expand exact source_refs only when needed, rerun the narrowest authoritative check after mutation, and do not repeat an identical failed repair on an unchanged repository fingerprint; re-plan or escalate instead. Revalidate stale paths after repository drift.\\n{}",
'''
render_repl = '''        "[medusa-coding-trajectory-v1]\\nAuthoritative compact trajectory derived from the canonical journal. Preserve immutable objective/constraints and use repair_ledger as the complete actionable failure set. Repair all independent diagnostics from the latest verification generation together, expand exact source_refs only when needed, rerun the narrowest authoritative check after mutation, and do not repeat an identical failed repair on an unchanged repository fingerprint. When roadblocks are present, follow only an admissible selected_alternative, preserve authority boundaries, and materially change strategy; rejected alternatives and disproved hypotheses must not be retried without new evidence. If disposition is escalation_required, complete independent work and report the exact blocker rather than claiming success. Revalidate stale paths after repository drift.\\n{}",
'''
assert text.count(render_anchor) == 1
text = text.replace(render_anchor, render_repl)
coding.write_text(text)
