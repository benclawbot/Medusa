//! Conflict-aware decomposition and deterministic integration planning for mutating implementers.
//!
//! This module is deliberately policy-only: it never grants integration authority. It converts
//! exact mutation contracts into a durable DAG, computes exclusive ownership conflicts, exposes
//! deterministic runnable waves, and records the barrier/invalidation information required by the
//! runtime transaction layer.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const MUTATION_DAG_SCHEMA_VERSION: u16 = 1;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MutationResourceKind {
    Path,
    Symbol,
    PublicApi,
    GeneratedOutput,
    Manifest,
    Lockfile,
    Migration,
    FeatureFlag,
    Snapshot,
}

#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct MutationResource {
    pub kind: MutationResourceKind,
    pub key: String,
}

impl MutationResource {
    pub fn new(kind: MutationResourceKind, key: impl Into<String>) -> Result<Self, &'static str> {
        let key = normalize_resource_key(kind, &key.into());
        if key.is_empty() {
            return Err("mutation resource key cannot be empty");
        }
        Ok(Self { kind, key })
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct MutationTaskContract {
    pub id: String,
    pub repository_revision: String,
    pub resources: Vec<MutationResource>,
    pub dependencies: Vec<String>,
    pub capabilities: Vec<String>,
    pub required_evidence: Vec<String>,
    pub verification_responsibility: Vec<String>,
    pub confidence_milli: u16,
}

impl MutationTaskContract {
    fn canonicalize(mut self) -> Result<Self, &'static str> {
        self.id = self.id.trim().to_owned();
        self.repository_revision = self.repository_revision.trim().to_owned();
        if self.id.is_empty() || self.repository_revision.is_empty() {
            return Err("mutation task requires id and repository revision");
        }
        if self.confidence_milli > 1_000 {
            return Err("mutation task confidence is outside the accepted range");
        }
        self.resources = self
            .resources
            .into_iter()
            .map(|resource| MutationResource::new(resource.kind, resource.key))
            .collect::<Result<Vec<_>, _>>()?;
        self.resources.sort();
        self.resources.dedup();
        if self.resources.is_empty() {
            return Err("mutation task requires exact owned resources");
        }
        canonical_strings(&mut self.dependencies);
        canonical_strings(&mut self.capabilities);
        canonical_strings(&mut self.required_evidence);
        canonical_strings(&mut self.verification_responsibility);
        if self.required_evidence.is_empty() || self.verification_responsibility.is_empty() {
            return Err("mutation task requires evidence and verification responsibility");
        }
        if self.dependencies.iter().any(|dependency| dependency == &self.id) {
            return Err("mutation task cannot depend on itself");
        }
        Ok(self)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConflictReason {
    OverlappingPath,
    SharedSymbol,
    SharedPublicApi,
    SharedGeneratedOutput,
    SharedManifest,
    SharedLockfile,
    SharedMigration,
    SharedFeatureFlag,
    SharedSnapshot,
}

#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct ConflictEdge {
    pub first: String,
    pub second: String,
    pub reasons: Vec<ConflictReason>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct MutationDag {
    pub schema_version: u16,
    pub repository_revision: String,
    pub tasks: Vec<MutationTaskContract>,
    pub conflict_edges: Vec<ConflictEdge>,
    pub max_parallelism: u16,
    pub fingerprint: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DecompositionDecision {
    Parallel(MutationDag),
    SingleImplementer { reason: String },
}

impl MutationDag {
    pub fn build(
        tasks: Vec<MutationTaskContract>,
        max_parallelism: u16,
        minimum_confidence_milli: u16,
    ) -> Result<DecompositionDecision, &'static str> {
        if tasks.is_empty() {
            return Err("mutation decomposition requires at least one task");
        }
        if max_parallelism == 0 || minimum_confidence_milli > 1_000 {
            return Err("mutation decomposition limits are invalid");
        }
        let mut canonical = tasks
            .into_iter()
            .map(MutationTaskContract::canonicalize)
            .collect::<Result<Vec<_>, _>>()?;
        canonical.sort_by(|left, right| left.id.cmp(&right.id));
        if canonical
            .windows(2)
            .any(|window| window[0].id == window[1].id)
        {
            return Err("mutation task ids must be unique");
        }
        let repository_revision = canonical[0].repository_revision.clone();
        if canonical
            .iter()
            .any(|task| task.repository_revision != repository_revision)
        {
            return Ok(DecompositionDecision::SingleImplementer {
                reason: "mutation tasks do not share one immutable repository revision".to_owned(),
            });
        }
        if canonical
            .iter()
            .any(|task| task.confidence_milli < minimum_confidence_milli)
        {
            return Ok(DecompositionDecision::SingleImplementer {
                reason: "mutation decomposition confidence is below policy threshold".to_owned(),
            });
        }
        let ids = canonical
            .iter()
            .map(|task| task.id.as_str())
            .collect::<BTreeSet<_>>();
        if canonical.iter().any(|task| {
            task.dependencies
                .iter()
                .any(|dependency| !ids.contains(dependency.as_str()))
        }) {
            return Ok(DecompositionDecision::SingleImplementer {
                reason: "mutation dependency references an unaccepted task".to_owned(),
            });
        }
        validate_acyclic(&canonical)?;
        if canonical.len() < 2 || max_parallelism < 2 {
            return Ok(DecompositionDecision::SingleImplementer {
                reason: "parallel mutation requires at least two accepted tasks and workers"
                    .to_owned(),
            });
        }
        let conflict_edges = conflict_edges(&canonical);
        let mut dag = Self {
            schema_version: MUTATION_DAG_SCHEMA_VERSION,
            repository_revision,
            tasks: canonical,
            conflict_edges,
            max_parallelism,
            fingerprint: String::new(),
        };
        dag.fingerprint = dag_fingerprint(&dag);
        dag.validate()?;
        if dag.maximum_runnable_width() < 2 {
            return Ok(DecompositionDecision::SingleImplementer {
                reason: "accepted mutation contracts have no conflict-free parallel wave"
                    .to_owned(),
            });
        }
        Ok(DecompositionDecision::Parallel(dag))
    }

    pub fn validate(&self) -> Result<(), &'static str> {
        if self.schema_version != MUTATION_DAG_SCHEMA_VERSION
            || self.repository_revision.trim().is_empty()
            || self.tasks.is_empty()
            || self.max_parallelism == 0
            || self.fingerprint != dag_fingerprint(self)
        {
            return Err("mutation DAG is incomplete or corrupted");
        }
        if self
            .tasks
            .iter()
            .any(|task| task.repository_revision != self.repository_revision)
        {
            return Err("mutation DAG spans more than one repository revision");
        }
        validate_acyclic(&self.tasks)?;
        if self.conflict_edges != conflict_edges(&self.tasks) {
            return Err("mutation DAG conflict edges do not match owned resources");
        }
        Ok(())
    }

    #[must_use]
    pub fn deterministic_integration_order(&self) -> Vec<String> {
        deterministic_topological_order(&self.tasks)
    }

    #[must_use]
    pub fn runnable_wave(&self, completed: &BTreeSet<String>) -> Vec<String> {
        let mut ready = self
            .tasks
            .iter()
            .filter(|task| {
                !completed.contains(&task.id)
                    && task
                        .dependencies
                        .iter()
                        .all(|dependency| completed.contains(dependency))
            })
            .map(|task| task.id.clone())
            .collect::<Vec<_>>();
        ready.sort();

        let mut selected = Vec::new();
        for candidate in ready {
            if selected.len() >= usize::from(self.max_parallelism) {
                break;
            }
            if selected
                .iter()
                .all(|accepted| !self.conflicts(candidate.as_str(), accepted))
            {
                selected.push(candidate);
            }
        }
        selected
    }

    #[must_use]
    pub fn conflicts(&self, first: &str, second: &str) -> bool {
        let (first, second) = ordered_pair(first, second);
        self.conflict_edges
            .iter()
            .any(|edge| edge.first == first && edge.second == second)
    }

    #[must_use]
    pub fn maximum_runnable_width(&self) -> usize {
        let mut completed = BTreeSet::new();
        let mut maximum = 0;
        while completed.len() < self.tasks.len() {
            let wave = self.runnable_wave(&completed);
            if wave.is_empty() {
                break;
            }
            maximum = maximum.max(wave.len());
            completed.extend(wave);
        }
        maximum
    }

    pub fn recompute_after_scope_change(
        &self,
        task_id: &str,
        resources: Vec<MutationResource>,
    ) -> Result<ScopeChangeDecision, &'static str> {
        let mut tasks = self.tasks.clone();
        let task = tasks
            .iter_mut()
            .find(|task| task.id == task_id)
            .ok_or("scope change references an unknown mutation task")?;
        task.resources = resources;
        *task = task.clone().canonicalize()?;
        let new_edges = conflict_edges(&tasks);
        let old_edges = self.conflict_edges.iter().cloned().collect::<BTreeSet<_>>();
        let introduced = new_edges
            .iter()
            .filter(|edge| !old_edges.contains(*edge))
            .cloned()
            .collect::<Vec<_>>();
        if introduced.is_empty() {
            return Ok(ScopeChangeDecision::Continue {
                resources_fingerprint: resources_fingerprint(&task.resources),
            });
        }
        let mut affected = introduced
            .iter()
            .flat_map(|edge| [edge.first.clone(), edge.second.clone()])
            .collect::<Vec<_>>();
        affected.sort();
        affected.dedup();
        Ok(ScopeChangeDecision::ReplanAndSerialize {
            affected_tasks: affected,
            introduced_conflicts: introduced,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ScopeChangeDecision {
    Continue {
        resources_fingerprint: String,
    },
    ReplanAndSerialize {
        affected_tasks: Vec<String>,
        introduced_conflicts: Vec<ConflictEdge>,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AcceptedTaskEvidence {
    pub task_id: String,
    pub prepared_commit: String,
    pub prepared_tree: String,
    pub contract_fingerprint: String,
    pub dependency_fingerprints: BTreeMap<String, String>,
    pub verification_fingerprint: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct IntegrationBarrier {
    pub dag_fingerprint: String,
    pub ordered_tasks: Vec<String>,
    pub accepted: BTreeMap<String, AcceptedTaskEvidence>,
    pub fingerprint: String,
}

impl IntegrationBarrier {
    pub fn establish(
        dag: &MutationDag,
        evidence: Vec<AcceptedTaskEvidence>,
    ) -> Result<Self, &'static str> {
        dag.validate()?;
        let accepted = evidence
            .into_iter()
            .map(|item| (item.task_id.clone(), item))
            .collect::<BTreeMap<_, _>>();
        if accepted.len() != dag.tasks.len()
            || dag.tasks.iter().any(|task| !accepted.contains_key(&task.id))
        {
            return Err("integration barrier requires accepted evidence for every DAG task");
        }
        for task in &dag.tasks {
            let item = accepted
                .get(&task.id)
                .ok_or("integration barrier lost accepted task evidence")?;
            if item.prepared_commit.trim().is_empty()
                || item.prepared_tree.trim().is_empty()
                || item.contract_fingerprint.trim().is_empty()
                || item.verification_fingerprint.trim().is_empty()
            {
                return Err("accepted mutation evidence is incomplete");
            }
            if task.dependencies.iter().any(|dependency| {
                accepted
                    .get(dependency)
                    .and_then(|upstream| item.dependency_fingerprints.get(dependency).map(|seen| (seen, upstream)))
                    .is_none_or(|(seen, upstream)| seen != &upstream.prepared_tree)
            }) {
                return Err("downstream evidence is stale against accepted upstream inputs");
            }
        }
        let ordered_tasks = dag.deterministic_integration_order();
        let mut barrier = Self {
            dag_fingerprint: dag.fingerprint.clone(),
            ordered_tasks,
            accepted,
            fingerprint: String::new(),
        };
        barrier.fingerprint = barrier_fingerprint(&barrier);
        Ok(barrier)
    }

    #[must_use]
    pub fn stale_downstream_after_upstream_change(
        &self,
        upstream_task: &str,
        new_tree: &str,
    ) -> Vec<String> {
        let mut stale = self
            .accepted
            .values()
            .filter(|evidence| {
                evidence
                    .dependency_fingerprints
                    .get(upstream_task)
                    .is_some_and(|tree| tree != new_tree)
            })
            .map(|evidence| evidence.task_id.clone())
            .collect::<Vec<_>>();
        stale.sort();
        stale
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct BatchIntegrationReceipt {
    pub barrier_fingerprint: String,
    pub base_head: String,
    pub integrated_tasks: Vec<String>,
    pub final_head: String,
    pub final_tree: String,
    pub final_verification_fingerprint: String,
    pub fingerprint: String,
}

impl BatchIntegrationReceipt {
    pub fn complete(
        barrier: &IntegrationBarrier,
        base_head: impl Into<String>,
        integrated_tasks: Vec<String>,
        final_head: impl Into<String>,
        final_tree: impl Into<String>,
        final_verification_fingerprint: impl Into<String>,
    ) -> Result<Self, &'static str> {
        let base_head = base_head.into();
        let final_head = final_head.into();
        let final_tree = final_tree.into();
        let final_verification_fingerprint = final_verification_fingerprint.into();
        if integrated_tasks != barrier.ordered_tasks
            || base_head.trim().is_empty()
            || final_head.trim().is_empty()
            || final_tree.trim().is_empty()
            || final_verification_fingerprint.trim().is_empty()
        {
            return Err("batch integration receipt does not prove deterministic completion");
        }
        let mut receipt = Self {
            barrier_fingerprint: barrier.fingerprint.clone(),
            base_head,
            integrated_tasks,
            final_head,
            final_tree,
            final_verification_fingerprint,
            fingerprint: String::new(),
        };
        receipt.fingerprint = batch_receipt_fingerprint(&receipt);
        Ok(receipt)
    }

    #[must_use]
    pub fn reconciles(&self, barrier: &IntegrationBarrier, primary_head: &str) -> bool {
        self.barrier_fingerprint == barrier.fingerprint
            && self.integrated_tasks == barrier.ordered_tasks
            && self.final_head == primary_head
            && self.fingerprint == batch_receipt_fingerprint(self)
    }
}

fn conflict_edges(tasks: &[MutationTaskContract]) -> Vec<ConflictEdge> {
    let mut edges = Vec::new();
    for (index, left) in tasks.iter().enumerate() {
        for right in tasks.iter().skip(index + 1) {
            let mut reasons = BTreeSet::new();
            for left_resource in &left.resources {
                for right_resource in &right.resources {
                    if resources_overlap(left_resource, right_resource) {
                        reasons.insert(conflict_reason(left_resource.kind));
                    }
                }
            }
            if !reasons.is_empty() {
                let (first, second) = ordered_pair(&left.id, &right.id);
                edges.push(ConflictEdge {
                    first: first.to_owned(),
                    second: second.to_owned(),
                    reasons: reasons.into_iter().collect(),
                });
            }
        }
    }
    edges.sort();
    edges
}

fn resources_overlap(left: &MutationResource, right: &MutationResource) -> bool {
    if left.kind != right.kind {
        return false;
    }
    if left.kind != MutationResourceKind::Path {
        return left.key == right.key;
    }
    path_overlaps(&left.key, &right.key)
}

fn path_overlaps(left: &str, right: &str) -> bool {
    left == right
        || left
            .strip_prefix(right)
            .is_some_and(|suffix| suffix.starts_with('/'))
        || right
            .strip_prefix(left)
            .is_some_and(|suffix| suffix.starts_with('/'))
}

const fn conflict_reason(kind: MutationResourceKind) -> ConflictReason {
    match kind {
        MutationResourceKind::Path => ConflictReason::OverlappingPath,
        MutationResourceKind::Symbol => ConflictReason::SharedSymbol,
        MutationResourceKind::PublicApi => ConflictReason::SharedPublicApi,
        MutationResourceKind::GeneratedOutput => ConflictReason::SharedGeneratedOutput,
        MutationResourceKind::Manifest => ConflictReason::SharedManifest,
        MutationResourceKind::Lockfile => ConflictReason::SharedLockfile,
        MutationResourceKind::Migration => ConflictReason::SharedMigration,
        MutationResourceKind::FeatureFlag => ConflictReason::SharedFeatureFlag,
        MutationResourceKind::Snapshot => ConflictReason::SharedSnapshot,
    }
}

fn validate_acyclic(tasks: &[MutationTaskContract]) -> Result<(), &'static str> {
    if deterministic_topological_order(tasks).len() == tasks.len() {
        Ok(())
    } else {
        Err("mutation dependency graph contains a cycle")
    }
}

fn deterministic_topological_order(tasks: &[MutationTaskContract]) -> Vec<String> {
    let mut remaining = tasks
        .iter()
        .map(|task| (task.id.clone(), task.dependencies.clone()))
        .collect::<BTreeMap<_, _>>();
    let mut completed = BTreeSet::new();
    let mut ordered = Vec::with_capacity(tasks.len());
    loop {
        let next = remaining
            .iter()
            .find(|(_, dependencies)| {
                dependencies
                    .iter()
                    .all(|dependency| completed.contains(dependency))
            })
            .map(|(id, _)| id.clone());
        let Some(next) = next else {
            break;
        };
        remaining.remove(&next);
        completed.insert(next.clone());
        ordered.push(next);
    }
    ordered
}

fn normalize_resource_key(kind: MutationResourceKind, value: &str) -> String {
    let trimmed = value.trim().replace('\\', "/");
    if kind == MutationResourceKind::Path {
        trimmed
            .split('/')
            .filter(|part| !part.is_empty() && *part != ".")
            .collect::<Vec<_>>()
            .join("/")
    } else {
        trimmed
    }
}

fn canonical_strings(values: &mut Vec<String>) {
    values.retain(|value| !value.trim().is_empty());
    for value in values.iter_mut() {
        *value = value.trim().to_owned();
    }
    values.sort();
    values.dedup();
}

fn ordered_pair<'a>(left: &'a str, right: &'a str) -> (&'a str, &'a str) {
    if left <= right {
        (left, right)
    } else {
        (right, left)
    }
}

fn resources_fingerprint(resources: &[MutationResource]) -> String {
    hash(resources)
}

fn dag_fingerprint(dag: &MutationDag) -> String {
    hash(&(
        dag.schema_version,
        &dag.repository_revision,
        &dag.tasks,
        &dag.conflict_edges,
        dag.max_parallelism,
    ))
}

fn barrier_fingerprint(barrier: &IntegrationBarrier) -> String {
    hash(&(
        &barrier.dag_fingerprint,
        &barrier.ordered_tasks,
        &barrier.accepted,
    ))
}

fn batch_receipt_fingerprint(receipt: &BatchIntegrationReceipt) -> String {
    hash(&(
        &receipt.barrier_fingerprint,
        &receipt.base_head,
        &receipt.integrated_tasks,
        &receipt.final_head,
        &receipt.final_tree,
        &receipt.final_verification_fingerprint,
    ))
}

fn hash<T: Serialize + ?Sized>(value: &T) -> String {
    let bytes = serde_json::to_vec(value).unwrap_or_default();
    let mut digest = Sha256::new();
    digest.update(bytes);
    hex::encode(digest.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resource(kind: MutationResourceKind, key: &str) -> MutationResource {
        MutationResource::new(kind, key).expect("test resource must be valid")
    }

    fn task(id: &str, resources: Vec<MutationResource>, dependencies: &[&str]) -> MutationTaskContract {
        MutationTaskContract {
            id: id.to_owned(),
            repository_revision: "base-revision".to_owned(),
            resources,
            dependencies: dependencies.iter().map(|value| (*value).to_owned()).collect(),
            capabilities: vec!["repository-write".to_owned()],
            required_evidence: vec!["changed-paths".to_owned()],
            verification_responsibility: vec!["targeted-tests".to_owned()],
            confidence_milli: 930,
        }
    }

    fn parallel(tasks: Vec<MutationTaskContract>) -> MutationDag {
        match MutationDag::build(tasks, 4, 850).expect("test DAG must build") {
            DecompositionDecision::Parallel(dag) => dag,
            DecompositionDecision::SingleImplementer { reason } => {
                panic!("expected parallel DAG, got fallback: {reason}")
            }
        }
    }

    #[test]
    fn disjoint_packages_run_in_same_wave() {
        let dag = parallel(vec![
            task("api", vec![resource(MutationResourceKind::Path, "crates/api")], &[]),
            task("ui", vec![resource(MutationResourceKind::Path, "crates/ui")], &[]),
        ]);
        assert_eq!(dag.runnable_wave(&BTreeSet::new()), vec!["api", "ui"]);
        assert!(dag.conflict_edges.is_empty());
    }

    #[test]
    fn shared_manifest_serializes_ownership_and_falls_back_without_parallel_wave() {
        let decision = MutationDag::build(
            vec![
                task("api", vec![resource(MutationResourceKind::Manifest, "Cargo.toml")], &[]),
                task("ui", vec![resource(MutationResourceKind::Manifest, "Cargo.toml")], &[]),
            ],
            4,
            850,
        )
        .expect("conflicting DAG must be analyzable");
        assert!(matches!(decision, DecompositionDecision::SingleImplementer { .. }));
    }

    #[test]
    fn generated_output_conflicts_even_when_source_paths_are_disjoint() {
        let decision = MutationDag::build(
            vec![
                task(
                    "one",
                    vec![
                        resource(MutationResourceKind::Path, "packages/one"),
                        resource(MutationResourceKind::GeneratedOutput, "schema/client.rs"),
                    ],
                    &[],
                ),
                task(
                    "two",
                    vec![
                        resource(MutationResourceKind::Path, "packages/two"),
                        resource(MutationResourceKind::GeneratedOutput, "schema/client.rs"),
                    ],
                    &[],
                ),
                task("three", vec![resource(MutationResourceKind::Path, "packages/three")], &[]),
            ],
            4,
            850,
        )
        .expect("mixed DAG must be analyzable");
        let DecompositionDecision::Parallel(dag) = decision else {
            panic!("third disjoint task should preserve a parallel wave");
        };
        assert!(dag.conflicts("one", "two"));
        assert_eq!(dag.runnable_wave(&BTreeSet::new()).len(), 2);
    }

    #[test]
    fn scope_expansion_introduces_conflict_and_requests_replan() {
        let dag = parallel(vec![
            task("api", vec![resource(MutationResourceKind::Path, "crates/api")], &[]),
            task("ui", vec![resource(MutationResourceKind::Path, "crates/ui")], &[]),
        ]);
        let decision = dag
            .recompute_after_scope_change(
                "ui",
                vec![resource(MutationResourceKind::Path, "crates")],
            )
            .expect("scope recomputation must succeed");
        let ScopeChangeDecision::ReplanAndSerialize { affected_tasks, .. } = decision else {
            panic!("scope expansion must introduce a conflict");
        };
        assert_eq!(affected_tasks, vec!["api", "ui"]);
    }

    #[test]
    fn integration_order_is_independent_of_worker_completion_order() {
        let dag = parallel(vec![
            task("b", vec![resource(MutationResourceKind::Path, "b")], &[]),
            task("a", vec![resource(MutationResourceKind::Path, "a")], &[]),
            task("c", vec![resource(MutationResourceKind::Path, "c")], &["a"]),
        ]);
        assert_eq!(dag.deterministic_integration_order(), vec!["a", "b", "c"]);
    }

    #[test]
    fn stale_downstream_evidence_blocks_barrier() {
        let dag = parallel(vec![
            task("a", vec![resource(MutationResourceKind::Path, "a")], &[]),
            task("b", vec![resource(MutationResourceKind::Path, "b")], &["a"]),
            task("c", vec![resource(MutationResourceKind::Path, "c")], &[]),
        ]);
        let evidence = vec![
            accepted("a", "tree-a", BTreeMap::new()),
            accepted("b", "tree-b", BTreeMap::from([("a".to_owned(), "old-tree".to_owned())])),
            accepted("c", "tree-c", BTreeMap::new()),
        ];
        assert!(IntegrationBarrier::establish(&dag, evidence).is_err());
    }

    #[test]
    fn barrier_and_batch_receipt_are_deterministic_and_reconcilable() {
        let dag = parallel(vec![
            task("a", vec![resource(MutationResourceKind::Path, "a")], &[]),
            task("b", vec![resource(MutationResourceKind::Path, "b")], &["a"]),
            task("c", vec![resource(MutationResourceKind::Path, "c")], &[]),
        ]);
        let barrier = IntegrationBarrier::establish(
            &dag,
            vec![
                accepted("c", "tree-c", BTreeMap::new()),
                accepted("b", "tree-b", BTreeMap::from([("a".to_owned(), "tree-a".to_owned())])),
                accepted("a", "tree-a", BTreeMap::new()),
            ],
        )
        .expect("accepted evidence must establish the barrier");
        assert_eq!(barrier.ordered_tasks, vec!["a", "b", "c"]);
        let receipt = BatchIntegrationReceipt::complete(
            &barrier,
            "base",
            barrier.ordered_tasks.clone(),
            "final-head",
            "final-tree",
            "verification",
        )
        .expect("complete integration must produce a receipt");
        assert!(receipt.reconciles(&barrier, "final-head"));
        assert!(!receipt.reconciles(&barrier, "partial-head"));
    }

    #[test]
    fn ambiguous_or_low_confidence_decomposition_stays_single_implementer() {
        let mut low = task("a", vec![resource(MutationResourceKind::Path, "a")], &[]);
        low.confidence_milli = 700;
        let decision = MutationDag::build(
            vec![low, task("b", vec![resource(MutationResourceKind::Path, "b")], &[])],
            4,
            850,
        )
        .expect("fallback must be a valid decision");
        assert!(matches!(decision, DecompositionDecision::SingleImplementer { .. }));
    }

    fn accepted(
        id: &str,
        tree: &str,
        dependency_fingerprints: BTreeMap<String, String>,
    ) -> AcceptedTaskEvidence {
        AcceptedTaskEvidence {
            task_id: id.to_owned(),
            prepared_commit: format!("commit-{id}"),
            prepared_tree: tree.to_owned(),
            contract_fingerprint: format!("contract-{id}"),
            dependency_fingerprints,
            verification_fingerprint: format!("verification-{id}"),
        }
    }
}
