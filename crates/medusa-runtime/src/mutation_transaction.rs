//! Dedicated parent-review transaction facade.

use std::{
    path::Path,
    sync::{atomic::AtomicBool, mpsc::Sender},
};

use medusa_config::Config;
use medusa_provider::ModelProvider;

use crate::RuntimeEvent;

#[path = "mutation_transaction_state.rs"]
mod state;

pub use state::*;
pub(crate) use crate::parent_reviewer::ParentReviewAuthorization;

pub fn authorize_after_parent_review<P: ModelProvider>(
    path: &Path,
    repo: &Path,
    provider: &P,
    config: &Config,
    cancel: &AtomicBool,
    events: &Sender<RuntimeEvent>,
) -> Result<ParentReviewAuthorization, String> {
    crate::parent_reviewer::authorize(path, repo, provider, config, cancel, events)
}

pub fn complete_after_parent_review<P: ModelProvider>(
    path: &Path,
    repo: &Path,
    provider: &P,
    config: &Config,
    cancel: &AtomicBool,
    events: &Sender<RuntimeEvent>,
) -> Result<TransactionCompletion, String> {
    crate::parent_reviewer::complete(path, repo, provider, config, cancel, events)
}
