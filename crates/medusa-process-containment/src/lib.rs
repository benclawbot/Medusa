//! Cross-platform boundaries for safe external-process ownership.

#[cfg(windows)]
extern crate self as flatbuffers;

#[cfg(windows)]
// SAFETY: reviewed Windows composable-sandbox FFI; see the checked allowlist.
#[allow(unsafe_code)]
mod base_container;
#[cfg(windows)]
mod flatbuffer_builder;
// SAFETY: reviewed native process-identity FFI is isolated in this low-level crate.
#[allow(unsafe_code)]
mod process_identity;
#[cfg(windows)]
// SAFETY: reviewed Windows Job Object/process FFI; see the checked allowlist.
#[allow(unsafe_code)]
mod windows;
#[cfg(windows)]
// SAFETY: reviewed Windows token and ACL FFI; see the checked allowlist.
#[allow(unsafe_code)]
mod windows_acl;

#[cfg(windows)]
pub use base_container::{
    WindowsSandboxRestrictions, run_appcontainer, run_appcontainer_cancellable,
};
#[cfg(windows)]
pub(crate) use flatbuffer_builder::FlatBufferBuilder;
pub use process_identity::{
    NativeProcessStartMarker, ProcessOwnershipReceipt, ProcessOwnershipVerification,
    process_start_marker,
};
#[cfg(windows)]
pub use windows::{WindowsJob, process_is_alive};
#[cfg(windows)]
pub use windows_acl::{secure_current_user_only, verify_current_user_only};
