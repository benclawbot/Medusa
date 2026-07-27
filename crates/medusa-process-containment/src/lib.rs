//! Cross-platform boundaries for safe external-process ownership.

#[cfg(windows)]
extern crate self as flatbuffers;

#[cfg(windows)]
mod base_container;
#[cfg(windows)]
mod flatbuffer_builder;
#[cfg(windows)]
mod windows;

#[cfg(windows)]
pub(crate) use flatbuffer_builder::FlatBufferBuilder;
#[cfg(windows)]
pub use base_container::{WindowsSandboxRestrictions, run_appcontainer};
#[cfg(windows)]
pub use windows::{WindowsJob, process_is_alive};
