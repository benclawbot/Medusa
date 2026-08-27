//! TUI runtime adapter. Execution authority stays behind `DaemonSupervisor`
//! and commands cross the process boundary as `FrontendCommandEnvelope`.

#[path = "runtime_inner.rs"]
mod inner;

pub use inner::*;
