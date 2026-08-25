//! Cross-platform boundaries for safe external-process ownership.

#[cfg(windows)]
extern crate self as flatbuffers;

// SAFETY: reviewed FlatBuffers parser accessors are verifier-bound and isolated in this module.
// The builder is consumed by the Windows sandbox path; Linux/macOS retain the parser regression
// test but do not construct production builders.
#[cfg_attr(not(windows), allow(dead_code))]
#[allow(unsafe_code)]
mod flatbuffer_builder;

#[cfg(windows)]
// SAFETY: reviewed Windows composable-sandbox FFI; see the checked allowlist.
#[allow(unsafe_code)]
mod base_container;
// SAFETY: reviewed native process-identity FFI is isolated in this low-level crate.
#[allow(unsafe_code)]
mod process_identity;
// SAFETY: reviewed process-group signal FFI is isolated in this low-level crate.
#[allow(unsafe_code)]
mod process_tree;
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
    WindowsSandboxLimits, WindowsSandboxRestrictions, run_appcontainer,
    run_appcontainer_cancellable, run_appcontainer_cancellable_observed,
};
#[cfg(windows)]
pub(crate) use flatbuffer_builder::FlatBufferBuilder;
pub use process_identity::{
    NativeProcessStartMarker, ProcessOwnershipReceipt, ProcessOwnershipVerification,
    process_start_marker,
};
pub use process_tree::OwnedProcessTree;
#[cfg(windows)]
pub use windows::{WindowsJob, process_is_alive};
#[cfg(windows)]
pub use windows_acl::{secure_current_user_only, verify_current_user_only};
