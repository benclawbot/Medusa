//! Versioned capability authority shared by model, CLI, UI, protocol, and documentation surfaces.

mod registry;

pub use registry::*;

pub mod explicit;
pub use explicit::*;
