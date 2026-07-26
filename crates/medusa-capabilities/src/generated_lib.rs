include!("lib.rs");

#[path = "product_capabilities.rs"]
pub mod product_capabilities;

pub use product_capabilities::{
    AuditRecord, CapabilityDescriptor, CapabilityPermission, GitHubCapability,
    ProductCapability, SelfImprovementTransaction, TransactionState,
};
