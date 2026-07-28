#[path = "action.rs"]
pub mod action;
#[path = "lib.rs"]
mod legacy;

pub use action::{
    AuthorizedRecoveryAction, RecoveryActionRejection, RecoveryActionRequest,
};
pub use legacy::*;
