from pathlib import Path

path = Path("crates/medusa-runtime/src/checkpoint_payload.rs")
source = path.read_text()
source = source.replace(
    "    use medusa_core::MedusaResult;\n",
    "    use medusa_core::{CorrelationId, MedusaResult, SessionId};\n",
)
old = '''        let event = medusa_protocol::EventEnvelope::new(
            medusa_protocol::SessionId::parse("session-1").expect("session"),
            0,
            Actor::Coordinator,
            medusa_protocol::CorrelationId::parse("correlation-1").expect("correlation"),
            None,
            EventPayload::FileTransactionCommitted {
                paths: vec!["binary.bin".to_owned()],
                rollback_ref: "rollback".to_owned(),
            },
        )
'''
new = '''        let event = medusa_protocol::EventEnvelope::new(
            0,
            SessionId::parse("session-1").expect("session"),
            Actor::Coordinator,
            CorrelationId::parse("correlation-1").expect("correlation"),
            EventPayload::FileTransactionCommitted {
                paths: vec!["binary.bin".to_owned()],
                rollback_ref: "rollback".to_owned(),
            },
            None,
            time::OffsetDateTime::UNIX_EPOCH,
        )
'''
if new not in source:
    count = source.count(old)
    if count != 1:
        raise SystemExit(f"checkpoint payload test constructor target changed: {count}")
    source = source.replace(old, new, 1)
path.write_text(source)
