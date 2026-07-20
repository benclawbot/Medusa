//! Cross-platform boundaries for safe external-process ownership.

#[cfg(windows)]
mod windows;

#[cfg(windows)]
pub use windows::{WindowsJob, process_is_alive};
