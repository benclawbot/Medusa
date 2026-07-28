#[path = "model.rs"]
mod model;
mod snapshot_builder;

pub use model::*;
pub use snapshot_builder::{
    ChangeEvidence, HunkEvidence, ReviewSnapshotBuildError, ReviewSnapshotInput,
    build_review_snapshot,
};
