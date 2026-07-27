//! Cross-platform boundaries for safe external-process ownership.

#[cfg(windows)]
mod base_container;
#[cfg(windows)]
mod windows;

#[cfg(windows)]
pub use base_container::{WindowsSandboxRestrictions, run_appcontainer};
#[cfg(windows)]
pub use windows::{WindowsJob, process_is_alive};
