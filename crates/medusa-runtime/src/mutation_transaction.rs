//! Dedicated parent-review transaction facade.

#[path = "mutation_transaction_state.rs"]
mod state;

pub(crate) use crate::parent_reviewer::ParentReviewAuthorization;
pub use state::*;
