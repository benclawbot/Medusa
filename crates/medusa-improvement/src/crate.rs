//! Guarded self-improvement and evidence-backed learning primitives.

mod legacy {
    include!("lib.rs");
}

pub use legacy::*;
pub mod learning;
