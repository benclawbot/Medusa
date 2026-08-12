//! Telegram compatibility wrapper over the shared frontend projection authority.

use medusa_protocol::{
    EventEnvelope,
    frontend::{FrontendEventEnvelope, FrontendKind},
};

pub fn project_event(
    event: &EventEnvelope,
    presentation_cursor: u64,
) -> Option<FrontendEventEnvelope> {
    medusa_protocol::frontend::project_event(event, presentation_cursor, FrontendKind::Telegram)
}
