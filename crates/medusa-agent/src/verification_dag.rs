use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

#[path = "verification_resource_pool.rs"]
pub mod resource_pool;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum VerificationAuthority {
    Diagnostic,
    IndependentAcceptance,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum VerificationNodeState {
    Pending,
    Running,
    Passed,
    Failed,
    Stale,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerificationInputKey {
    pub repository_revision: String,
    pub tree_fingerprint: String,
    pub environment_fingerprint: String,
    pub toolchain_fingerprint: String,
    pub adapter_version: String,
    pub changed_paths: BTreeSet<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerificationNode {
    pub id: String,
    pub command: String,
    pub dependencies: BTreeSet<String>,
    pub authority: VerificationAuthority,
    pub expected_duration_ms: u64,
    pub resource_class: String,
    pub input: VerificationInputKey,
    pub state: VerificationNodeState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerificationReceipt {
    pub node_id: String,
    pub input: VerificationInputKey,
    pub passed: bool,
    pub duration_ms: u64,
    pub artifact_refs: Vec<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct VerificationRecovery {
    pub restored_passed: usize,
    pub restored_failed: usize,
    pub requeued_running: usize,
    pub restored_stale: usize,
    pub restored_pending: usize,
    pub restored_cancelled: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerificationDag {
    nodes: BTreeMap<String, VerificationNode>,
    receipts: BTreeMap<String, VerificationReceipt>,
}

impl VerificationDag {
    pub fn insert(&mut self, node: VerificationNode) -> Result<(), String> {
        if node.id.trim().is_empty() {
            return Err("verification node id must not be empty".to_owned());
        }
        if node.command.trim().is_empty() {
            return Err(format!("verification node {} has no command", node.id));
        }
        if node.dependencies.contains(&node.id) {
            return Err(format!("verification node {} depends on itself", node.id));
        }
        if node.state != VerificationNodeState::Pending {
            return Err(format!(
                "verification node {} must be inserted pending",
                node.id
            ));
        }
        if self.nodes.contains_key(&node.id) {
            return Err(format!("duplicate verification node {}", node.id));
        }
        for dependency in &node.dependencies {
            if !self.nodes.contains_key(dependency) {
                return Err(format!(
                    "verification node {} references unknown dependency {dependency}",
                    node.id
                ));
            }
        }
        self.nodes.insert(node.id.clone(), node);
        Ok(())
    }

    pub fn node(&self, id: &str) -> Option<&VerificationNode> {
        self.nodes.get(id)
    }

    pub fn ready_nodes(&self) -> Vec<&VerificationNode> {
        self.nodes
            .values()
            .filter(|node| {
                node.state == VerificationNodeState::Pending
                    && node.dependencies.iter().all(|dependency| {
                        self.nodes.get(dependency).is_some_and(|dependency| {
                            dependency.state == VerificationNodeState::Passed
                                && self.receipts.get(&dependency.id).is_some_and(|receipt| {
                                    receipt.passed && receipt.input == dependency.input
                                })
                        })
                    })
            })
            .collect()
    }

    pub fn mark_running(&mut self, id: &str) -> Result<(), String> {
        let ready = self.ready_nodes().iter().any(|node| node.id == id);
        if !ready {
            return Err(format!("verification node {id} is not ready"));
        }
        let node = self
            .nodes
            .get_mut(id)
            .ok_or_else(|| format!("verification node {id} disappeared while becoming ready"))?;
        node.state = VerificationNodeState::Running;
        Ok(())
    }

    pub fn record_receipt(&mut self, receipt: VerificationReceipt) -> Result<(), String> {
        let node = self
            .nodes
            .get_mut(&receipt.node_id)
            .ok_or_else(|| format!("unknown verification node {}", receipt.node_id))?;
        if node.state != VerificationNodeState::Running {
            return Err(format!(
                "verification node {} is not running",
                receipt.node_id
            ));
        }
        if node.input != receipt.input {
            node.state = VerificationNodeState::Stale;
            self.receipts.remove(&receipt.node_id);
            return Err(format!(
                "verification receipt input mismatch for {}",
                receipt.node_id
            ));
        }
        node.state = if receipt.passed {
            VerificationNodeState::Passed
        } else {
            VerificationNodeState::Failed
        };
        self.receipts.insert(receipt.node_id.clone(), receipt);
        Ok(())
    }

    pub fn reusable_receipt(
        &self,
        node_id: &str,
        input: &VerificationInputKey,
    ) -> Option<&VerificationReceipt> {
        let node = self.nodes.get(node_id)?;
        let receipt = self.receipts.get(node_id)?;
        (node.state == VerificationNodeState::Passed
            && node.input == *input
            && receipt.input == *input
            && receipt.passed)
            .then_some(receipt)
    }

    pub fn invalidate_paths<'a>(&mut self, paths: impl IntoIterator<Item = &'a str>) -> usize {
        let changed = paths.into_iter().collect::<BTreeSet<_>>();
        if changed.is_empty() {
            return 0;
        }

        let mut invalidated = BTreeSet::new();
        for node in self.nodes.values() {
            if node
                .input
                .changed_paths
                .iter()
                .any(|path| changed.contains(path.as_str()))
            {
                invalidated.insert(node.id.clone());
            }
        }

        loop {
            let before = invalidated.len();
            for node in self.nodes.values() {
                if node
                    .dependencies
                    .iter()
                    .any(|dependency| invalidated.contains(dependency))
                {
                    invalidated.insert(node.id.clone());
                }
            }
            if invalidated.len() == before {
                break;
            }
        }

        for id in &invalidated {
            if let Some(node) = self.nodes.get_mut(id) {
                node.state = VerificationNodeState::Stale;
            }
            self.receipts.remove(id);
        }
        invalidated.len()
    }

    pub fn refresh_stale_input(
        &mut self,
        id: &str,
        input: VerificationInputKey,
    ) -> Result<(), String> {
        let node = self
            .nodes
            .get_mut(id)
            .ok_or_else(|| format!("unknown verification node {id}"))?;
        if node.state != VerificationNodeState::Stale {
            return Err(format!("verification node {id} is not stale"));
        }
        node.input = input;
        node.state = VerificationNodeState::Pending;
        self.receipts.remove(id);
        Ok(())
    }

    pub fn exact_receipts_reusable_from(&self, prior: &Self) -> bool {
        !self.nodes.is_empty()
            && self.nodes.len() == prior.nodes.len()
            && self.nodes.iter().all(|(id, node)| {
                prior
                    .reusable_receipt(id, &node.input)
                    .is_some_and(|receipt| receipt.node_id == *id)
                    && prior.nodes.get(id).is_some_and(|prior_node| {
                        prior_node.command == node.command
                            && prior_node.dependencies == node.dependencies
                            && prior_node.authority == node.authority
                            && prior_node.resource_class == node.resource_class
                    })
            })
    }

    pub fn recover_for_restart(
        &mut self,
        persisted: &Self,
    ) -> Result<VerificationRecovery, String> {
        if self.nodes.len() != persisted.nodes.len()
            || !self.nodes.iter().all(|(id, node)| {
                persisted.nodes.get(id).is_some_and(|persisted_node| {
                    persisted_node.id == node.id
                        && persisted_node.command == node.command
                        && persisted_node.dependencies == node.dependencies
                        && persisted_node.authority == node.authority
                        && persisted_node.expected_duration_ms == node.expected_duration_ms
                        && persisted_node.resource_class == node.resource_class
                        && persisted_node.input == node.input
                })
            })
        {
            return Err("persisted verification DAG definition or inputs drifted".to_owned());
        }

        let mut recovery = VerificationRecovery::default();
        self.receipts.clear();

        for (id, node) in &mut self.nodes {
            let persisted_node = persisted
                .nodes
                .get(id)
                .ok_or_else(|| format!("persisted verification node {id} disappeared"))?;
            match persisted_node.state {
                VerificationNodeState::Passed => {
                    if let Some(receipt) = persisted.receipts.get(id).filter(|receipt| {
                        receipt.node_id == *id && receipt.input == node.input && receipt.passed
                    }) {
                        node.state = VerificationNodeState::Passed;
                        self.receipts.insert(id.clone(), receipt.clone());
                        recovery.restored_passed += 1;
                    } else {
                        node.state = VerificationNodeState::Pending;
                        recovery.restored_pending += 1;
                    }
                }
                VerificationNodeState::Failed => {
                    if let Some(receipt) = persisted.receipts.get(id).filter(|receipt| {
                        receipt.node_id == *id && receipt.input == node.input && !receipt.passed
                    }) {
                        node.state = VerificationNodeState::Failed;
                        self.receipts.insert(id.clone(), receipt.clone());
                        recovery.restored_failed += 1;
                    } else {
                        node.state = VerificationNodeState::Pending;
                        recovery.restored_pending += 1;
                    }
                }
                VerificationNodeState::Running => {
                    node.state = VerificationNodeState::Pending;
                    recovery.requeued_running += 1;
                }
                VerificationNodeState::Stale => {
                    node.state = VerificationNodeState::Stale;
                    recovery.restored_stale += 1;
                }
                VerificationNodeState::Pending => {
                    node.state = VerificationNodeState::Pending;
                    recovery.restored_pending += 1;
                }
                VerificationNodeState::Cancelled => {
                    node.state = VerificationNodeState::Cancelled;
                    recovery.restored_cancelled += 1;
                }
            }
        }

        loop {
            let invalidated = self
                .nodes
                .values()
                .filter(|node| {
                    matches!(
                        node.state,
                        VerificationNodeState::Passed | VerificationNodeState::Failed
                    ) && node.dependencies.iter().any(|dependency| {
                        self.nodes.get(dependency).is_none_or(|dependency_node| {
                            dependency_node.state != VerificationNodeState::Passed
                                || self.receipts.get(dependency).is_none_or(|receipt| {
                                    !receipt.passed || receipt.input != dependency_node.input
                                })
                        })
                    })
                })
                .map(|node| node.id.clone())
                .collect::<Vec<_>>();
            if invalidated.is_empty() {
                break;
            }
            for id in invalidated {
                if let Some(node) = self.nodes.get_mut(&id) {
                    match node.state {
                        VerificationNodeState::Passed => {
                            recovery.restored_passed = recovery.restored_passed.saturating_sub(1);
                        }
                        VerificationNodeState::Failed => {
                            recovery.restored_failed = recovery.restored_failed.saturating_sub(1);
                        }
                        _ => {}
                    }
                    node.state = VerificationNodeState::Pending;
                    recovery.restored_pending += 1;
                }
                self.receipts.remove(&id);
            }
        }

        Ok(recovery)
    }

    pub fn authoritative_complete(&self) -> bool {
        let authoritative = self
            .nodes
            .values()
            .filter(|node| node.authority == VerificationAuthority::IndependentAcceptance)
            .collect::<Vec<_>>();
        !authoritative.is_empty()
            && authoritative.iter().all(|node| {
                node.state == VerificationNodeState::Passed
                    && self
                        .receipts
                        .get(&node.id)
                        .is_some_and(|receipt| receipt.passed && receipt.input == node.input)
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(paths: &[&str]) -> VerificationInputKey {
        VerificationInputKey {
            repository_revision: "rev-a".to_owned(),
            tree_fingerprint: "tree-a".to_owned(),
            environment_fingerprint: "env-a".to_owned(),
            toolchain_fingerprint: "rust-a".to_owned(),
            adapter_version: "adapter-v1".to_owned(),
            changed_paths: paths.iter().map(|path| (*path).to_owned()).collect(),
        }
    }

    fn node(
        id: &str,
        dependencies: &[&str],
        authority: VerificationAuthority,
        paths: &[&str],
    ) -> VerificationNode {
        VerificationNode {
            id: id.to_owned(),
            command: format!("check-{id}"),
            dependencies: dependencies
                .iter()
                .map(|dependency| (*dependency).to_owned())
                .collect(),
            authority,
            expected_duration_ms: 10,
            resource_class: "cpu".to_owned(),
            input: input(paths),
            state: VerificationNodeState::Pending,
        }
    }

    fn pass(dag: &mut VerificationDag, id: &str) {
        dag.mark_running(id).expect("node ready");
        let input = dag.node(id).expect("node exists").input.clone();
        dag.record_receipt(VerificationReceipt {
            node_id: id.to_owned(),
            input,
            passed: true,
            duration_ms: 4,
            artifact_refs: vec![format!("artifact:{id}")],
        })
        .expect("receipt accepted");
    }

    #[test]
    fn dependencies_gate_parallel_readiness_and_authority() {
        let mut dag = VerificationDag::default();
        dag.insert(node(
            "format",
            &[],
            VerificationAuthority::Diagnostic,
            &["src/lib.rs"],
        ))
        .expect("format node");
        dag.insert(node(
            "unit",
            &["format"],
            VerificationAuthority::IndependentAcceptance,
            &["src/lib.rs"],
        ))
        .expect("unit node");

        assert_eq!(
            dag.ready_nodes()
                .iter()
                .map(|node| node.id.as_str())
                .collect::<Vec<_>>(),
            vec!["format"]
        );
        assert!(!dag.authoritative_complete());
        pass(&mut dag, "format");
        assert_eq!(dag.ready_nodes()[0].id, "unit");
        pass(&mut dag, "unit");
        assert!(dag.authoritative_complete());
    }

    #[test]
    fn exact_input_receipts_are_reusable_but_drift_fails_closed() {
        let mut dag = VerificationDag::default();
        dag.insert(node(
            "unit",
            &[],
            VerificationAuthority::IndependentAcceptance,
            &["src/lib.rs"],
        ))
        .expect("unit node");
        pass(&mut dag, "unit");

        let exact = input(&["src/lib.rs"]);
        assert!(dag.reusable_receipt("unit", &exact).is_some());
        let mut drifted = exact.clone();
        drifted.toolchain_fingerprint = "rust-b".to_owned();
        assert!(dag.reusable_receipt("unit", &drifted).is_none());
    }

    #[test]
    fn complete_exact_receipt_set_is_reusable_but_command_or_input_drift_is_not() {
        let mut prior = VerificationDag::default();
        prior
            .insert(node(
                "unit",
                &[],
                VerificationAuthority::IndependentAcceptance,
                &["src/lib.rs"],
            ))
            .expect("unit node");
        pass(&mut prior, "unit");

        let mut current = VerificationDag::default();
        current
            .insert(node(
                "unit",
                &[],
                VerificationAuthority::IndependentAcceptance,
                &["src/lib.rs"],
            ))
            .expect("unit node");
        assert!(current.exact_receipts_reusable_from(&prior));

        current.nodes.get_mut("unit").unwrap().command = "different-command".to_owned();
        assert!(!current.exact_receipts_reusable_from(&prior));
        current.nodes.get_mut("unit").unwrap().command = "check-unit".to_owned();
        current
            .nodes
            .get_mut("unit")
            .unwrap()
            .input
            .environment_fingerprint = "env-b".to_owned();
        assert!(!current.exact_receipts_reusable_from(&prior));
    }

    #[test]
    fn restart_recovery_preserves_evidenced_completion_and_requeues_running_work() {
        let mut persisted = VerificationDag::default();
        persisted
            .insert(node(
                "format",
                &[],
                VerificationAuthority::Diagnostic,
                &["src/lib.rs"],
            ))
            .expect("format node");
        persisted
            .insert(node(
                "unit",
                &["format"],
                VerificationAuthority::IndependentAcceptance,
                &["src/lib.rs"],
            ))
            .expect("unit node");
        pass(&mut persisted, "format");
        persisted.mark_running("unit").expect("unit running");

        let mut current = VerificationDag::default();
        current
            .insert(node(
                "format",
                &[],
                VerificationAuthority::Diagnostic,
                &["src/lib.rs"],
            ))
            .expect("format node");
        current
            .insert(node(
                "unit",
                &["format"],
                VerificationAuthority::IndependentAcceptance,
                &["src/lib.rs"],
            ))
            .expect("unit node");

        let recovery = current.recover_for_restart(&persisted).expect("recover");
        assert_eq!(recovery.restored_passed, 1);
        assert_eq!(recovery.requeued_running, 1);
        assert_eq!(
            current.node("format").unwrap().state,
            VerificationNodeState::Passed
        );
        assert_eq!(
            current.node("unit").unwrap().state,
            VerificationNodeState::Pending
        );
        assert_eq!(current.ready_nodes()[0].id, "unit");
    }

    #[test]
    fn restart_recovery_requeues_passed_descendants_of_interrupted_prerequisites() {
        let mut persisted = VerificationDag::default();
        persisted
            .insert(node(
                "format",
                &[],
                VerificationAuthority::Diagnostic,
                &["src/lib.rs"],
            ))
            .expect("format node");
        persisted
            .insert(node(
                "unit",
                &["format"],
                VerificationAuthority::IndependentAcceptance,
                &["src/lib.rs"],
            ))
            .expect("unit node");
        persisted.mark_running("format").expect("format running");
        persisted.nodes.get_mut("unit").unwrap().state = VerificationNodeState::Passed;
        let unit_input = persisted.node("unit").unwrap().input.clone();
        persisted.receipts.insert(
            "unit".to_owned(),
            VerificationReceipt {
                node_id: "unit".to_owned(),
                input: unit_input,
                passed: true,
                duration_ms: 4,
                artifact_refs: vec!["artifact:unit".to_owned()],
            },
        );

        let mut current = VerificationDag::default();
        current
            .insert(node(
                "format",
                &[],
                VerificationAuthority::Diagnostic,
                &["src/lib.rs"],
            ))
            .expect("format node");
        current
            .insert(node(
                "unit",
                &["format"],
                VerificationAuthority::IndependentAcceptance,
                &["src/lib.rs"],
            ))
            .expect("unit node");

        let recovery = current.recover_for_restart(&persisted).expect("recover");
        assert_eq!(recovery.requeued_running, 1);
        assert_eq!(recovery.restored_passed, 0);
        assert_eq!(recovery.restored_pending, 1);
        assert_eq!(current.node("format").unwrap().state, VerificationNodeState::Pending);
        assert_eq!(current.node("unit").unwrap().state, VerificationNodeState::Pending);
        assert!(!current.authoritative_complete());
    }

    #[test]
    fn restart_recovery_rejects_definition_or_input_drift() {
        let mut persisted = VerificationDag::default();
        persisted
            .insert(node(
                "unit",
                &[],
                VerificationAuthority::IndependentAcceptance,
                &["src/lib.rs"],
            ))
            .expect("unit node");

        let mut current = persisted.clone();
        current.nodes.get_mut("unit").unwrap().command = "different-command".to_owned();
        assert!(current.recover_for_restart(&persisted).is_err());

        let mut current = persisted.clone();
        current
            .nodes
            .get_mut("unit")
            .unwrap()
            .input
            .tree_fingerprint = "tree-b".to_owned();
        assert!(current.recover_for_restart(&persisted).is_err());
    }

    #[test]
    fn restart_recovery_does_not_trust_passed_state_without_matching_receipt() {
        let mut persisted = VerificationDag::default();
        persisted
            .insert(node(
                "unit",
                &[],
                VerificationAuthority::IndependentAcceptance,
                &["src/lib.rs"],
            ))
            .expect("unit node");
        persisted.nodes.get_mut("unit").unwrap().state = VerificationNodeState::Passed;

        let mut current = VerificationDag::default();
        current
            .insert(node(
                "unit",
                &[],
                VerificationAuthority::IndependentAcceptance,
                &["src/lib.rs"],
            ))
            .expect("unit node");
        let recovery = current.recover_for_restart(&persisted).expect("recover");
        assert_eq!(recovery.restored_pending, 1);
        assert_eq!(
            current.node("unit").unwrap().state,
            VerificationNodeState::Pending
        );
        assert!(!current.authoritative_complete());
    }

    #[test]
    fn restart_recovery_preserves_stale_and_pending_states_explicitly() {
        let mut persisted = VerificationDag::default();
        persisted
            .insert(node(
                "stale",
                &[],
                VerificationAuthority::Diagnostic,
                &["a.rs"],
            ))
            .expect("stale node");
        persisted
            .insert(node(
                "pending",
                &[],
                VerificationAuthority::IndependentAcceptance,
                &["b.rs"],
            ))
            .expect("pending node");
        persisted.nodes.get_mut("stale").unwrap().state = VerificationNodeState::Stale;

        let mut current = VerificationDag::default();
        current
            .insert(node(
                "stale",
                &[],
                VerificationAuthority::Diagnostic,
                &["a.rs"],
            ))
            .expect("stale node");
        current
            .insert(node(
                "pending",
                &[],
                VerificationAuthority::IndependentAcceptance,
                &["b.rs"],
            ))
            .expect("pending node");

        let recovery = current.recover_for_restart(&persisted).expect("recover");
        assert_eq!(recovery.restored_stale, 1);
        assert_eq!(recovery.restored_pending, 1);
        assert_eq!(
            current.node("stale").unwrap().state,
            VerificationNodeState::Stale
        );
        assert_eq!(
            current.node("pending").unwrap().state,
            VerificationNodeState::Pending
        );
    }

    #[test]
    fn path_invalidation_can_rearm_affected_nodes_without_losing_unrelated_receipts() {
        let mut dag = VerificationDag::default();
        dag.insert(node(
            "format-a",
            &[],
            VerificationAuthority::Diagnostic,
            &["a.rs"],
        ))
        .expect("format a");
        dag.insert(node(
            "test-a",
            &["format-a"],
            VerificationAuthority::IndependentAcceptance,
            &["a.rs"],
        ))
        .expect("test a");
        dag.insert(node(
            "test-b",
            &[],
            VerificationAuthority::IndependentAcceptance,
            &["b.rs"],
        ))
        .expect("test b");
        pass(&mut dag, "format-a");
        pass(&mut dag, "test-a");
        pass(&mut dag, "test-b");

        assert_eq!(dag.invalidate_paths(["a.rs"]), 2);
        assert_eq!(
            dag.node("format-a").unwrap().state,
            VerificationNodeState::Stale
        );
        assert_eq!(
            dag.node("test-a").unwrap().state,
            VerificationNodeState::Stale
        );
        assert_eq!(
            dag.node("test-b").unwrap().state,
            VerificationNodeState::Passed
        );
        assert!(dag.reusable_receipt("test-b", &input(&["b.rs"])).is_some());

        let mut refreshed = input(&["a.rs"]);
        refreshed.tree_fingerprint = "tree-b".to_owned();
        dag.refresh_stale_input("format-a", refreshed.clone())
            .expect("format rearmed");
        dag.refresh_stale_input("test-a", refreshed)
            .expect("test rearmed");
        assert_eq!(dag.ready_nodes()[0].id, "format-a");
        pass(&mut dag, "format-a");
        assert_eq!(dag.ready_nodes()[0].id, "test-a");
    }

    #[test]
    fn mismatched_receipt_marks_running_node_stale() {
        let mut dag = VerificationDag::default();
        dag.insert(node(
            "unit",
            &[],
            VerificationAuthority::IndependentAcceptance,
            &["src/lib.rs"],
        ))
        .expect("unit node");
        dag.mark_running("unit").expect("ready");
        let mut mismatched = input(&["src/lib.rs"]);
        mismatched.repository_revision = "rev-b".to_owned();
        let error = dag
            .record_receipt(VerificationReceipt {
                node_id: "unit".to_owned(),
                input: mismatched,
                passed: true,
                duration_ms: 1,
                artifact_refs: Vec::new(),
            })
            .expect_err("mismatch rejected");
        assert!(error.contains("input mismatch"));
        assert_eq!(
            dag.node("unit").unwrap().state,
            VerificationNodeState::Stale
        );
        assert!(!dag.authoritative_complete());
    }

    #[test]
    fn authoritative_completion_requires_matching_receipts_even_after_deserialization() {
        let mut dag = VerificationDag::default();
        dag.insert(node(
            "unit",
            &[],
            VerificationAuthority::IndependentAcceptance,
            &["src/lib.rs"],
        ))
        .expect("unit node");
        let mut serialized = serde_json::to_value(&dag).expect("serialize dag");
        serialized["nodes"]["unit"]["state"] = serde_json::json!("Passed");
        let restored: VerificationDag = serde_json::from_value(serialized).expect("restore dag");
        assert!(!restored.authoritative_complete());
    }

    #[test]
    fn insert_rejects_prepassed_nodes_without_receipts() {
        let mut dag = VerificationDag::default();
        let mut prepassed = node(
            "unit",
            &[],
            VerificationAuthority::IndependentAcceptance,
            &["src/lib.rs"],
        );
        prepassed.state = VerificationNodeState::Passed;
        assert!(dag.insert(prepassed).is_err());
    }
}
