//! Versioned capability authority shared by model, CLI, UI, protocol, and documentation surfaces.

extern crate self as medusa_browser_client;

mod registry;
pub mod verification_route;

pub use registry::*;

pub mod explicit;
pub use explicit::*;
