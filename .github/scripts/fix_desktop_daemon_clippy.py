#!/usr/bin/env python3
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]


def replace_once(path: str, old: str, new: str) -> None:
    target = ROOT / path
    content = target.read_text()
    count = content.count(old)
    if count != 1:
        raise SystemExit(f"{path}: expected one Clippy correction anchor, found {count}")
    target.write_text(content.replace(old, new, 1))


replace_once(
    "crates/medusa-daemon/src/telegram/command.rs",
    "    Forward(FrontendCommandEnvelope),\n",
    "    Forward(Box<FrontendCommandEnvelope>),\n",
)
replace_once(
    "crates/medusa-daemon/src/telegram/command.rs",
    "    Ok(TelegramInboundAction::Forward(envelope))\n",
    "    Ok(TelegramInboundAction::Forward(Box::new(envelope)))\n",
)
replace_once(
    "crates/medusa-daemon/src/telegram/service.rs",
    "                let acknowledgement = self.control.dispatch(envelope)?;\n",
    "                let acknowledgement = self.control.dispatch(*envelope)?;\n",
)

# The daemon owns submission ordering. Consume the desktop revision explicitly as advisory input
# while preserving the existing DTO contract during the authority migration.
replace_once(
    "apps/medusa-desktop/src-tauri/src/runtime.rs",
    '''    fn stage_draft(&mut self, draft: DesktopPromptDraft) -> Result<(String, Vec<String>), String> {
        let mut total = 0_usize;
''',
    '''    fn stage_draft(&mut self, draft: DesktopPromptDraft) -> Result<(String, Vec<String>), String> {
        let DesktopPromptDraft {
            text,
            attachments,
            revision,
        } = draft;
        let _advisory_revision = revision;
        let mut total = 0_usize;
''',
)
replace_once(
    "apps/medusa-desktop/src-tauri/src/runtime.rs",
    "        let mut ids = Vec::with_capacity(draft.attachments.len());\n        for attachment in draft.attachments {\n",
    "        let mut ids = Vec::with_capacity(attachments.len());\n        for attachment in attachments {\n",
)
replace_once(
    "apps/medusa-desktop/src-tauri/src/runtime.rs",
    "        Ok((draft.text, ids))\n",
    "        Ok((text, ids))\n",
)

# Frontend-supplied recovery evidence remains untrusted. Read and quarantine it explicitly so
# daemon-side recovery validation is authoritative without silently dropping the compatibility fields.
replace_once(
    "apps/medusa-desktop/src-tauri/src/runtime_recovery.rs",
    '''    let DesktopRecoveryActionRequest {
        recovery,
        operation,
        checkpoint_id,
        confirmed_destructive_effects,
        ..
    } = request;
''',
    '''    let DesktopRecoveryActionRequest {
        recovery,
        operation,
        checkpoint_id,
        confirmed_destructive_effects,
        repository_fingerprint_before,
        checkpoint_integrity_verified,
        repository_preconditions_verified,
        conflicting_uncommitted_paths,
        unresolved_risks,
    } = request;
    let _untrusted_frontend_evidence = (
        repository_fingerprint_before,
        checkpoint_integrity_verified,
        repository_preconditions_verified,
        conflicting_uncommitted_paths,
        unresolved_risks,
    );
''',
)
