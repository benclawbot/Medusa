//! Serializable control and presentation contracts shared by every Medusa frontend.

mod command;
mod event;
mod projection;

use crate::CURRENT_PROTOCOL_VERSION;

pub use command::{
    ApprovalDecision, AttachmentMode, FrontendCommand, FrontendCommandEnvelope, FrontendKind,
};
pub use event::{
    FrontendEvent, FrontendEventEnvelope, PresentationActivity, PresentationActivityKind,
    PresentationApproval, PresentationArtifact, PresentationLifecycle, PresentationPlanStep,
    PresentationQuestion, PresentationQuestionOption, PresentationWorker,
};

pub const FRONTEND_PROTOCOL_VERSION: crate::ProtocolVersion = CURRENT_PROTOCOL_VERSION;

pub use projection::project_event;
