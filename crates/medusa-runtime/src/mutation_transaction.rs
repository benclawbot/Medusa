//! Dedicated parent-review transaction facade.

use std::{
    collections::BTreeMap,
    path::Path,
    sync::{atomic::AtomicBool, mpsc::Sender},
};

use medusa_agent::AgentSession;
use medusa_config::Config;
use medusa_provider::ConfiguredProvider;

use crate::RuntimeEvent;

#[path = "mutation_transaction_legacy.rs"]
mod legacy;

pub use legacy::*;
pub(crate) use legacy::PARENT_REVIEW_TURN_INSTRUCTION;

pub fn complete_after_parent_review(
    path: &Path,
    repo: &Path,
    _session: &AgentSession,
    events: &Sender<RuntimeEvent>,
) -> Result<TransactionCompletion, String> {
    let project_config = repo.join(".medusa/config.toml");
    let project_config = project_config.is_file().then_some(project_config);
    let config = Config::load_layers(
        None,
        project_config.as_deref(),
        &BTreeMap::new(),
        &BTreeMap::new(),
    )
    .map_err(|error| error.to_string())?;
    let provider = ConfiguredProvider::manager_from_config(&config, None)
        .map_err(|error| error.to_string())?;
    let cancel = AtomicBool::new(false);
    crate::parent_reviewer::complete(path, repo, &provider, &config, &cancel, events)
}
