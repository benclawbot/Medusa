//! Versioned capability authority shared by model, CLI, UI, protocol, and documentation surfaces.

mod network_policy;
mod registry;

pub use network_policy::{ResolvedTarget, is_public_ip, resolve_public_target};
pub use registry::*;
