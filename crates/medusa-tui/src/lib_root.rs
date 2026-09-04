#![allow(clippy::type_complexity)]

// Keep the established TUI crate root intact while first-run onboarding is added as a
// separate reusable surface. The include preserves every existing module/test path.
include!("lib.rs");

pub mod setup;
