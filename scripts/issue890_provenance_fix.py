from pathlib import Path

path = Path("crates/medusa-improvement/src/provenance.rs")
text = path.read_text()
old = '''        EventPayload::ProviderExecutionRecorded { .. }\n        | EventPayload::ModelRequestStarted { .. }\n        | EventPayload::ModelResponseReceived { .. } => (\n            ProvenanceSource::ProviderExecution,\n            ProvenanceOutcome::Unresolved,\n            ProvenanceAuthority::SystemRecord,\n            "provider execution recorded".to_owned(),\n            None,\n        ),\n'''
new = '''        EventPayload::ModelRequestFailed { .. } => (\n            ProvenanceSource::ProviderExecution,\n            ProvenanceOutcome::Negative,\n            ProvenanceAuthority::SystemRecord,\n            "provider request failure recorded".to_owned(),\n            None,\n        ),\n        EventPayload::ProviderExecutionRecorded { .. }\n        | EventPayload::ModelRequestStarted { .. }\n        | EventPayload::ModelResponseReceived { .. } => (\n            ProvenanceSource::ProviderExecution,\n            ProvenanceOutcome::Unresolved,\n            ProvenanceAuthority::SystemRecord,\n            "provider execution recorded".to_owned(),\n            None,\n        ),\n'''
if text.count(old) != 1:
    raise SystemExit(f"expected one provider execution provenance arm, found {text.count(old)}")
path.write_text(text.replace(old, new, 1))
