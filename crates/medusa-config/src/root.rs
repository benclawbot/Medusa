#[path = "lib.rs"]
mod legacy;

pub use legacy::*;

mod store;

pub use store::{ApplyTiming, ConfigChangedEvent, ConfigStore, ConfigUpdate, RevisionedConfig};
