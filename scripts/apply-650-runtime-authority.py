from pathlib import Path
import re

# Ensure the worker exact-scope migration is part of this atomic integration gate.
exec(Path('scripts/apply-650-worker-scope.py').read_text(), {'__name__': '__main__'})
worker = Path('crates/medusa-workers/src/lib.rs')
text = worker.read_text()
text = text.replace(
    'let mut component = ChangedComponent::new(ChangeKind::Copied, path.clone())?;',
    '''let mut component = ChangedComponent::new(ChangeKind::Copied, path.clone())
                                .map_err(|error| invalid(error.to_string()))?;''',
    1,
)
text = text.replace(
    '''        normalize_components(&worker.worktree, &components)
            .map_err(|error| invalid(error.to_string()))
''',
    '''        if components.is_empty() {
            return Ok(Vec::new());
        }
        normalize_components(&worker.worktree, &components)
            .map_err(|error| invalid(error.to_string()))
''',
    1,
)
worker.write_text(text)

# Scope identity excludes content hashes, which belong to artifact evidence.
change = Path('crates/medusa-evidence/src/change.rs')
text = change.read_text()
old = '''pub fn changed_scope_fingerprint(components: &[ChangedComponent]) -> String {
    fingerprint(&components)
}
'''
new = '''pub fn changed_scope_fingerprint(components: &[ChangedComponent]) -> String {
    let mut scope = components.to_vec();
    for component in &mut scope {
        component.content_hash = None;
    }
    fingerprint(&scope)
}
'''
if text.count(old) != 1:
    raise SystemExit('scope fingerprint anchor missing')
change.write_text(text.replace(old, new, 1))

# Runtime and scheduler depend on the shared authority.
for manifest_name in ['crates/medusa-runtime/Cargo.toml', 'crates/medusa-multi-agent-scheduler/Cargo.toml']:
    manifest = Path(manifest_name)
    text = manifest.read_text()
    if 'medusa-evidence.workspace = true\n' not in text:
        marker = '[dependencies]\n'
        if marker not in text:
            raise SystemExit(f'dependency anchor missing in {manifest_name}')
        text = text.replace(marker, marker + 'medusa-evidence.workspace = true\n', 1)
    manifest.write_text(text)

# Worktree coordinator stores exact components and a typed receipt.
coordinator = Path('crates/medusa-runtime/src/mutating_worker_coordinator.rs')
text = coordinator.read_text()
text = text.replace(
    '''use medusa_agent::{
    AgentEngine, AgentExecutionPolicy, AgentUpdate, StepOutcome, TeamMemberContext, TeamRole,
    TeamRuntime, WorkerExecutionController, targeted_verification,
};
''',
    '''use medusa_agent::{
    AgentEngine, AgentExecutionPolicy, AgentUpdate, StepOutcome, TeamMemberContext, TeamRole,
    TeamRuntime, WorkerExecutionController, authoritative_verification_for_components,
};
use medusa_evidence::{ChangedComponent, VerificationReceipt, changed_scope_fingerprint};
''',
    1,
)
text = text.replace(
    '''    changed_paths: Vec<String>,
    verification_evidence: Vec<String>,
''',
    '''    changed_paths: Vec<String>,
    #[serde(default)]
    changed_components: Vec<ChangedComponent>,
    verification_evidence: Vec<String>,
    #[serde(default)]
    verification_receipt: Option<VerificationReceipt>,
''',
    1,
)
text = text.replace(
    '''    pub changed_paths: Vec<String>,
    pub verification_evidence: Vec<String>,
''',
    '''    pub changed_paths: Vec<String>,
    pub changed_components: Vec<ChangedComponent>,
    pub verification_evidence: Vec<String>,
    pub verification_receipt: VerificationReceipt,
''',
    1,
)
text = text.replace(
    '''        self.changed_paths,
        self.verification_evidence,
        self.review_context,
''',
    '''        self.changed_paths,
        self.verification_evidence,
        self.review_context,
''',
    1,
)
# Add a compact scope projection helper.
anchor = '''fn bounded_implementer_turns(configured: u32) -> u32 {
    configured.clamp(1, IMPLEMENTER_TURN_LIMIT)
}
'''
helper = '''fn bounded_implementer_turns(configured: u32) -> u32 {
    configured.clamp(1, IMPLEMENTER_TURN_LIMIT)
}

fn component_paths(components: &[ChangedComponent]) -> Vec<String> {
    let mut paths = components
        .iter()
        .flat_map(ChangedComponent::all_paths)
        .map(str::to_owned)
        .collect::<Vec<_>>();
    paths.sort();
    paths.dedup();
    paths
}
'''
if text.count(anchor) != 1:
    raise SystemExit('coordinator helper anchor missing')
text = text.replace(anchor, helper, 1)
text = text.replace(
    '''            changed_paths: Vec::new(),
            verification_evidence: Vec::new(),
''',
    '''            changed_paths: Vec::new(),
            changed_components: Vec::new(),
            verification_evidence: Vec::new(),
            verification_receipt: None,
''',
    1,
)
start = text.index('        let changed_paths = match manager.changed_paths_since(&worker, &base_head) {')
end = text.index('        let finalized = match manager.finalize_worker(', start)
replacement = '''        let changed_components = match manager.changed_components_since(&worker, &base_head) {
            Ok(components) => components,
            Err(error) => {
                let retryable = attempt < MAX_ATTEMPTS;
                let recorded = record_attempt_failure(
                    &mut controller,
                    team,
                    events,
                    control,
                    manager,
                    state_path,
                    &assignment,
                    &worker,
                    running,
                    Some(&run),
                    Vec::new(),
                    Vec::new(),
                    format!("failed to inspect isolated changes: {error}"),
                    retryable,
                    false,
                )?;
                last_error = Some(recorded.clone());
                if retryable {
                    continue;
                }
                return Err(recorded);
            }
        };
        let changed_paths = component_paths(&changed_components);
        if let Err(error) = validate_changed_paths(&contract, &changed_paths) {
            let retryable = attempt < MAX_ATTEMPTS;
            let recorded = record_attempt_failure(
                &mut controller,
                team,
                events,
                control,
                manager,
                state_path,
                &assignment,
                &worker,
                running,
                Some(&run),
                changed_paths,
                Vec::new(),
                error,
                retryable,
                false,
            )?;
            last_error = Some(recorded.clone());
            if retryable {
                continue;
            }
            return Err(recorded);
        }
        let worktree_identity = format!(
            "worktree:{}:{}",
            base_head,
            changed_scope_fingerprint(&changed_components)
        );
        let verification = match authoritative_verification_for_components(
            &worker.worktree,
            &preflight.repository_fingerprint,
            &worktree_identity,
            &changed_components,
        ) {
            Ok(verification) => verification,
            Err(error) => {
                let retryable = attempt < MAX_ATTEMPTS;
                let recorded = record_attempt_failure(
                    &mut controller,
                    team,
                    events,
                    control,
                    manager,
                    state_path,
                    &assignment,
                    &worker,
                    running,
                    Some(&run),
                    changed_paths,
                    Vec::new(),
                    format!("isolated verification could not run: {error}"),
                    retryable,
                    false,
                )?;
                last_error = Some(recorded.clone());
                if retryable {
                    continue;
                }
                return Err(recorded);
            }
        };
        if !verification.receipt.passed {
            let error = format!(
                "isolated worktree verification failed: {}",
                verification.summary.join(" | ")
            );
            let retryable = attempt < MAX_ATTEMPTS;
            let recorded = record_attempt_failure(
                &mut controller,
                team,
                events,
                control,
                manager,
                state_path,
                &assignment,
                &worker,
                running,
                Some(&run),
                changed_paths,
                verification.summary,
                error,
                retryable,
                false,
            )?;
            last_error = Some(recorded.clone());
            if retryable {
                continue;
            }
            return Err(recorded);
        }
        let verification_summary = verification.summary.clone();
        let verification_receipt = verification.receipt;
        if let Err(error) = manager.discard_untracked_runtime_state(&worker, &base_head) {
            let retryable = attempt < MAX_ATTEMPTS;
            let recorded = record_attempt_failure(
                &mut controller,
                team,
                events,
                control,
                manager,
                state_path,
                &assignment,
                &worker,
                running,
                Some(&run),
                changed_paths,
                verification_summary,
                format!("failed to clean runtime state after verification: {error}"),
                retryable,
                false,
            )?;
            last_error = Some(recorded.clone());
            if retryable {
                continue;
            }
            return Err(recorded);
        }
        let verified_components = match manager.changed_components_since(&worker, &base_head) {
            Ok(components) => components,
            Err(error) => {
                let retryable = attempt < MAX_ATTEMPTS;
                let recorded = record_attempt_failure(
                    &mut controller,
                    team,
                    events,
                    control,
                    manager,
                    state_path,
                    &assignment,
                    &worker,
                    running,
                    Some(&run),
                    changed_paths,
                    verification_summary,
                    format!("failed to inspect changes after verification: {error}"),
                    retryable,
                    false,
                )?;
                last_error = Some(recorded.clone());
                if retryable {
                    continue;
                }
                return Err(recorded);
            }
        };
        if changed_scope_fingerprint(&verified_components)
            != changed_scope_fingerprint(&changed_components)
        {
            let error = format!(
                "verification mutated the isolated worktree scope: before={changed_components:?}; after={verified_components:?}"
            );
            let retryable = attempt < MAX_ATTEMPTS;
            let recorded = record_attempt_failure(
                &mut controller,
                team,
                events,
                control,
                manager,
                state_path,
                &assignment,
                &worker,
                running,
                Some(&run),
                component_paths(&verified_components),
                verification_summary,
                error,
                retryable,
                false,
            )?;
            last_error = Some(recorded.clone());
            if retryable {
                continue;
            }
            return Err(recorded);
        }

'''
text = text[:start] + replacement + text[end:]
# Remaining finalize failure consumes the new summary.
text = text.replace('                    verification.evidence,', '                    verification_summary.clone(),', 1)
text = text.replace(
    '''            changed_paths,
            verification_evidence: verification.evidence,
''',
    '''            changed_paths,
            changed_components,
            verification_evidence: verification_summary,
            verification_receipt: Some(verification_receipt),
''',
    1,
)
text = text.replace(
    '''            changed_paths: state.changed_paths.clone(),
            implementation_summary: state.summary.clone(),
            worktree_verification_evidence: state.verification_evidence.clone(),
''',
    '''            changed_paths: state.changed_paths.clone(),
            changed_components: state.changed_components.clone(),
            implementation_summary: state.summary.clone(),
            worktree_verification_evidence: state.verification_evidence.clone(),
            worktree_verification_receipt: state
                .verification_receipt
                .clone()
                .ok_or_else(|| "prepared implementation has no typed verification receipt".to_owned())?,
''',
    1,
)
coordinator.write_text(text)

# State projection requires typed fields.
support = Path('crates/medusa-runtime/src/mutating_worker_coordinator_support.rs')
text = support.read_text()
text = text.replace(
    '''        || state.changed_paths.is_empty()
        || state.verification_evidence.is_empty())
''',
    '''        || state.changed_paths.is_empty()
        || state.changed_components.is_empty()
        || state.verification_evidence.is_empty()
        || state.verification_receipt.is_none())
''',
    1,
)
text = text.replace(
    '''        changed_paths: state.changed_paths.clone(),
        verification_evidence: state.verification_evidence.clone(),
''',
    '''        changed_paths: state.changed_paths.clone(),
        changed_components: state.changed_components.clone(),
        verification_evidence: state.verification_evidence.clone(),
        verification_receipt: state
            .verification_receipt
            .clone()
            .ok_or_else(|| "prepared implementation has no typed verification receipt".to_owned())?,
''',
    1,
)
support.write_text(text)

# Transaction stores typed worktree and independent receipts and authorizes only validated evidence.
transaction = Path('crates/medusa-runtime/src/mutation_transaction.rs')
text = transaction.read_text()
text = text.replace(
    'use medusa_agent::{AgentSession, targeted_verification};\n',
    'use medusa_agent::{AgentSession, authoritative_verification_for_components};\nuse medusa_evidence::{ChangedComponent, VerificationReceipt, changed_scope_fingerprint};\n',
    1,
)
text = text.replace(
    '''    pub changed_paths: Vec<String>,
    pub evidence: Vec<String>,
''',
    '''    pub changed_paths: Vec<String>,
    pub changed_components: Vec<ChangedComponent>,
    pub evidence: Vec<String>,
    pub authority: VerificationReceipt,
''',
    1,
)
text = text.replace(
    '''    pub changed_paths: Vec<String>,
    pub implementation_summary: String,
    pub worktree_verification_evidence: Vec<String>,
''',
    '''    pub changed_paths: Vec<String>,
    pub changed_components: Vec<ChangedComponent>,
    pub implementation_summary: String,
    pub worktree_verification_evidence: Vec<String>,
    pub worktree_verification_receipt: VerificationReceipt,
''',
    1,
)
text = text.replace(
    '''    pub changed_paths: Vec<String>,
    pub patch_fingerprint: String,
''',
    '''    pub changed_paths: Vec<String>,
    pub changed_components: Vec<ChangedComponent>,
    pub patch_fingerprint: String,
''',
    1,
)
text = text.replace(
    '''    pub worktree_verification_evidence: Vec<String>,
    pub review: Option<ParentReviewReceipt>,
''',
    '''    pub worktree_verification_evidence: Vec<String>,
    pub worktree_verification_receipt: Option<VerificationReceipt>,
    pub review: Option<ParentReviewReceipt>,
''',
    1,
)
text = text.replace(
    '''                changed_paths: Vec::new(),
                patch_fingerprint: String::new(),
''',
    '''                changed_paths: Vec::new(),
                changed_components: Vec::new(),
                patch_fingerprint: String::new(),
''',
    1,
)
text = text.replace(
    '''                worktree_verification_evidence: Vec::new(),
                review: None,
''',
    '''                worktree_verification_evidence: Vec::new(),
                worktree_verification_receipt: None,
                review: None,
''',
    1,
)
text = text.replace(
    'exact changed-path scope {:?}, patch fingerprint',
    'exact changed-component scope {:?}, patch fingerprint',
    1,
)
text = text.replace('        self.state.changed_paths,\n', '        self.state.changed_components,\n', 1)
# Replace independent verification implementation and receipt recording.
start = text.index('    pub fn verify_independently(&mut self, repo: &Path) -> Result<Vec<String>, String> {')
end = text.index('    pub fn authorize(&mut self, repo: &Path) -> Result<(), String> {', start)
replacement = '''    pub fn verify_independently(&mut self, repo: &Path) -> Result<Vec<String>, String> {
        if self.state.lifecycle == MutationLifecycle::ReviewAccepted {
            self.begin_verification()?;
        }
        if self.state.lifecycle != MutationLifecycle::VerificationPending {
            return Err(format!(
                "independent verification requires verification_pending state, found {:?}",
                self.state.lifecycle
            ));
        }
        let manager = self.manager(repo)?;
        let components = manager
            .commit_changed_components(&self.state.prepared_commit)
            .map_err(|error| error.to_string())?;
        if changed_scope_fingerprint(&components)
            != changed_scope_fingerprint(&self.state.changed_components)
        {
            let reason = "prepared commit scope changed before verification".to_owned();
            self.fail(reason.clone())?;
            return Err(reason);
        }
        let verification_root = self.root.join("independent-verification-worktree");
        manager
            .materialize_detached_commit(&self.state.prepared_commit, &verification_root)
            .map_err(|error| error.to_string())?;
        let verification = authoritative_verification_for_components(
            &verification_root,
            &self.state.repository_fingerprint,
            &self.state.prepared_commit,
            &components,
        );
        let cleanup = manager
            .remove_detached_worktree(&verification_root)
            .map_err(|error| error.to_string());
        let verification = verification.map_err(|error| error.to_string());
        if let Err(error) = cleanup {
            self.fail(format!("independent verification cleanup failed: {error}"))?;
            return Err(error);
        }
        let verification = verification?;
        if !verification.receipt.passed {
            let reason = format!(
                "independent verification failed: {}",
                verification.summary.join(" | ")
            );
            self.fail(reason.clone())?;
            return Err(reason);
        }
        let summary = verification.summary.clone();
        self.record_authoritative_verification(verification.receipt, summary.clone())?;
        Ok(summary)
    }

    pub fn record_verification(
        &mut self,
        passed: bool,
        evidence: Vec<String>,
    ) -> Result<(), String> {
        if !passed {
            let reason = format!(
                "independent verification rejected the commit: {}",
                evidence.join(" | ")
            );
            self.fail(reason.clone())?;
            return Err(reason);
        }
        Err("untyped verification evidence cannot authorize integration".to_owned())
    }

    pub fn record_authoritative_verification(
        &mut self,
        authority: VerificationReceipt,
        evidence: Vec<String>,
    ) -> Result<(), String> {
        if self.state.lifecycle != MutationLifecycle::VerificationPending {
            return Err(format!(
                "verification receipt requires verification_pending state, found {:?}",
                self.state.lifecycle
            ));
        }
        authority.validate().map_err(|error| error.to_string())?;
        if !authority.passed
            || authority.plan.repository_fingerprint != self.state.repository_fingerprint
            || authority.plan.commit != self.state.prepared_commit
            || changed_scope_fingerprint(&authority.plan.components)
                != changed_scope_fingerprint(&self.state.changed_components)
            || evidence.is_empty()
        {
            let reason = "typed verification receipt does not match the prepared mutation".to_owned();
            self.fail(reason.clone())?;
            return Err(reason);
        }
        let mut receipt = IndependentVerificationReceipt {
            verifier: "runtime-independent-verifier".to_owned(),
            commit: self.state.prepared_commit.clone(),
            tree: self.state.prepared_tree.clone(),
            changed_paths: self.state.changed_paths.clone(),
            changed_components: self.state.changed_components.clone(),
            evidence,
            authority,
            fingerprint: String::new(),
        };
        receipt.fingerprint = verification_fingerprint(&receipt);
        self.state.verification = Some(receipt);
        self.transition(MutationLifecycle::Verified, None)
    }

'''
text = text[:start] + replacement + text[end:]
# Strengthen authorization.
needle = '''        if review.commit != self.state.prepared_commit
            || verification.commit != self.state.prepared_commit
            || review.tree != self.state.prepared_tree
            || verification.tree != self.state.prepared_tree
        {
'''
replacement = '''        verification.authority.validate().map_err(|error| error.to_string())?;
        if review.commit != self.state.prepared_commit
            || verification.commit != self.state.prepared_commit
            || review.tree != self.state.prepared_tree
            || verification.tree != self.state.prepared_tree
            || !verification.authority.passed
            || verification.authority.plan.commit != self.state.prepared_commit
            || verification.authority.plan.repository_fingerprint
                != self.state.repository_fingerprint
            || changed_scope_fingerprint(&verification.authority.plan.components)
                != changed_scope_fingerprint(&self.state.changed_components)
        {
'''
if text.count(needle) != 1:
    raise SystemExit('authorization substitution anchor missing')
text = text.replace(needle, replacement, 1)
# Prepare exact components and worktree receipt.
text = text.replace(
    '''        let changed_paths = manager
            .commit_changed_paths(&commit)
            .map_err(|error| error.to_string())?;
        if changed_paths != input.changed_paths {
''',
    '''        let changed_components = manager
            .commit_changed_components(&commit)
            .map_err(|error| error.to_string())?;
        let changed_paths = manager
            .commit_changed_paths(&commit)
            .map_err(|error| error.to_string())?;
        input
            .worktree_verification_receipt
            .validate()
            .map_err(|error| error.to_string())?;
        if changed_paths != input.changed_paths
            || changed_scope_fingerprint(&changed_components)
                != changed_scope_fingerprint(&input.changed_components)
            || !input.worktree_verification_receipt.passed
        {
''',
    1,
)
text = text.replace(
    '''        self.state.changed_paths = changed_paths;
        self.state.patch_fingerprint = hash(&patch);
        self.state.scope_fingerprint = hash(&self.state.changed_paths);
''',
    '''        self.state.changed_paths = changed_paths;
        self.state.changed_components = changed_components;
        self.state.patch_fingerprint = hash(&patch);
        self.state.scope_fingerprint = changed_scope_fingerprint(&self.state.changed_components);
''',
    1,
)
text = text.replace(
    '''        self.state.worktree_verification_evidence = input.worktree_verification_evidence;
        self.state.implementation_evidence_fingerprint = hash(&(
''',
    '''        self.state.worktree_verification_evidence = input.worktree_verification_evidence;
        self.state.worktree_verification_receipt = Some(input.worktree_verification_receipt);
        self.state.implementation_evidence_fingerprint = hash(&(
''',
    1,
)
text = text.replace(
    '''            changed_paths: self.state.changed_paths.clone(),
        })
''',
    '''            changed_paths: self.state.changed_paths.clone(),
            changed_components: self.state.changed_components.clone(),
        })
''',
    1,
)
text = text.replace(
    '''                || self.state.changed_paths.is_empty()
                || self.state.patch_fingerprint.trim().is_empty()
''',
    '''                || self.state.changed_paths.is_empty()
                || self.state.changed_components.is_empty()
                || self.state.patch_fingerprint.trim().is_empty()
''',
    1,
)
text = text.replace(
    '''        if let Some(verification) = self.state.verification.as_ref()
            && verification.fingerprint != verification_fingerprint(verification)
        {
''',
    '''        if let Some(worktree) = self.state.worktree_verification_receipt.as_ref() {
            worktree.validate().map_err(|error| error.to_string())?;
        }
        if let Some(verification) = self.state.verification.as_ref()
            && (verification.fingerprint != verification_fingerprint(verification)
                || verification.authority.validate().is_err())
        {
''',
    1,
)
# Transaction test fixture constructs typed worktree receipt.
text = text.replace(
    '''        let input = PreparedMutationInput {
            plan_fingerprint: "plan".to_owned(),
''',
    '''        let commit = worker.commit.clone().expect("commit");
        let changed_components = manager
            .commit_changed_components(&commit)
            .expect("components");
        let worktree_verification = authoritative_verification_for_components(
            &worker.worktree,
            "repo",
            &format!("worktree:{}", changed_scope_fingerprint(&changed_components)),
            &changed_components,
        )
        .expect("worktree verification");
        let input = PreparedMutationInput {
            plan_fingerprint: "plan".to_owned(),
''',
    1,
)
text = text.replace(
    '''            changed_paths: vec!["src/lib.rs".to_owned()],
            implementation_summary: "changed the value".to_owned(),
            worktree_verification_evidence: vec!["worktree verified".to_owned()],
''',
    '''            changed_paths: vec!["src/lib.rs".to_owned()],
            changed_components,
            implementation_summary: "changed the value".to_owned(),
            worktree_verification_evidence: worktree_verification.summary,
            worktree_verification_receipt: worktree_verification.receipt,
''',
    1,
)
# Successful tests now use the real independent authority.
text = text.replace(
    '''        transaction.begin_verification().expect("begin verification");
        transaction
            .record_verification(true, vec!["independent verification passed".to_owned()])
            .expect("verification");
''',
    '''        transaction.verify_independently(&repo).expect("verification");
''',
    1,
)
text = text.replace(
    '''        transaction.begin_verification().expect("begin verification");
        transaction
            .record_verification(true, vec!["verified".to_owned()])
            .expect("verification");
''',
    '''        transaction.verify_independently(&repo).expect("verification");
''',
    1,
)
text = text.replace(
    '''        transaction.begin_verification().expect("verification pending");
        transaction
            .record_verification(true, vec!["verified".to_owned()])
            .expect("verified");
''',
    '''        transaction.verify_independently(&repo).expect("verified");
''',
    1,
)
transaction.write_text(text)

# Scheduler persists only validated typed dependencies when requested.
scheduler = Path('crates/medusa-multi-agent-scheduler/src/lib.rs')
text = scheduler.read_text()
text = text.replace(
    'use serde::{Deserialize, Serialize};\n',
    'use medusa_evidence::{EvidenceBundle, EvidenceDependency};\nuse serde::{Deserialize, Serialize};\n',
    1,
)
# Locate the ledger succeed method and append a typed variant immediately after it.
anchor = '''    pub fn fail(&mut self, task_id: &str, reason: impl Into<String>) -> Result<(), String> {
'''
method = '''    pub fn succeed_with_evidence(
        &mut self,
        task_id: &str,
        evidence: impl Into<String>,
        dependency: &EvidenceDependency,
        bundle: &EvidenceBundle,
    ) -> Result<(), String> {
        dependency.validate(bundle).map_err(|error| error.to_string())?;
        self.succeed(
            task_id,
            format!(
                "{}\ntyped_evidence_dependency={}\nbundle={}",
                evidence.into(),
                dependency.fingerprint,
                bundle.fingerprint
            ),
        )
    }

'''
if text.count(anchor) != 1:
    raise SystemExit('scheduler fail anchor missing')
text = text.replace(anchor, method + anchor, 1)
test_anchor = '    #[test]\n    fn ledger_recovers_interrupted_running_tasks() {'
test = '''    #[test]
    fn scheduler_rejects_unverified_evidence_dependency() {
        use medusa_evidence::{
            EvidenceBundle, EvidenceKind, EvidenceRecord, VerificationStatus,
        };
        let directory = tempfile::tempdir().expect("tempdir");
        let plan = simple_plan("evidence-plan");
        let mut ledger = ExecutionLedger::open(directory.path().join("ledger.json"), &plan)
            .expect("ledger");
        ledger.start("implementation", &TaskContext {
            task_id: "implementation".to_owned(),
            plan_fingerprint: plan.fingerprint.clone(),
            intent: "test".to_owned(),
            requested_outcomes: vec!["test".to_owned()],
            affected_components: vec!["repository".to_owned()],
            scope: ExecutionScope::Repository,
            repository_fingerprint: "repo".to_owned(),
            capability_requirements: vec!["coding".to_owned()],
            risk: RiskLevel::Medium,
        }).expect("start");
        let mut bundle = EvidenceBundle::new("repo", "commit");
        let decision = EvidenceRecord::new(
            EvidenceKind::Decision,
            "accept",
            "repo",
            "commit",
            "reviewer",
            VerificationStatus::Unverified,
            Vec::new(),
        ).expect("decision");
        bundle.records.push(decision.clone());
        bundle.refresh();
        assert!(medusa_evidence::EvidenceDependency::from_bundle(
            &bundle,
            vec![decision.id],
        ).is_err());
    }

'''
if test_anchor not in text:
    raise SystemExit('scheduler test anchor missing')
text = text.replace(test_anchor, test + test_anchor, 1)
scheduler.write_text(text)
