pub mod verification_dag {
    pub use medusa_agent::verification_dag::*;
}

#[path = "../src/verification_checkpoint.rs"]
mod verification_checkpoint;

use std::collections::BTreeSet;

use verification_checkpoint::{VerificationCheckpoint, VerificationCheckpointStore};
use verification_dag::{
    VerificationAuthority, VerificationDag, VerificationInputKey, VerificationNode,
    VerificationNodeState,
};

fn dag() -> VerificationDag {
    let mut dag = VerificationDag::default();
    dag.insert(VerificationNode {
        id: "acceptance".to_owned(),
        command: "cargo test".to_owned(),
        dependencies: BTreeSet::new(),
        authority: VerificationAuthority::IndependentAcceptance,
        expected_duration_ms: 25,
        resource_class: "cpu".to_owned(),
        input: VerificationInputKey {
            repository_revision: "rev".to_owned(),
            tree_fingerprint: "tree".to_owned(),
            environment_fingerprint: "env".to_owned(),
            toolchain_fingerprint: "toolchain".to_owned(),
            adapter_version: "adapter".to_owned(),
            changed_paths: ["src/lib.rs".to_owned()].into_iter().collect(),
        },
        state: VerificationNodeState::Pending,
    })
    .expect("node");
    dag
}

#[test]
fn durable_checkpoint_generation_roundtrips() {
    let directory = tempfile::tempdir().expect("directory");
    let store = VerificationCheckpointStore::new(directory.path());
    store
        .save(&VerificationCheckpoint::new("state", dag(), vec!["completed"]).expect("checkpoint"))
        .expect("save checkpoint");

    let restored = store
        .load::<Vec<String>>("state")
        .expect("load")
        .expect("checkpoint");
    assert_eq!(restored.payload, vec!["completed"]);
    assert_eq!(
        restored.dag.node("acceptance").expect("node").state,
        VerificationNodeState::Pending
    );
}
