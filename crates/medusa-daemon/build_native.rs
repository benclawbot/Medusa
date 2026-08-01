use std::{fs, path::Path};

pub fn run() {
    patch_control_plane();
    patch_delivery();
    patch_service();
}

fn patch_control_plane() {
    let path = Path::new("src/frontend_control.rs");
    let mut source = read(path);
    replace_if_present(
        &mut source,
        "artifact_store::{FrontendArtifactInput, FrontendArtifactStore, FrontendArtifactStoreError},",
        "artifact_store::{\n        FrontendArtifactExport, FrontendArtifactInput, FrontendArtifactStore,\n        FrontendArtifactStoreError,\n    },",
    );
    let marker = "    /// Validates, serializes, and idempotently acknowledges one frontend command.\n";
    if !source.contains("pub fn export_attachment") {
        let method = "    /// Exports one verified opaque artifact for a native frontend delivery.\n    pub fn export_attachment(\n        &self,\n        artifact_id: &str,\n    ) -> Result<FrontendArtifactExport, FrontendControlError> {\n        self.artifacts.export(artifact_id).map_err(Into::into)\n    }\n\n";
        replace_required(&mut source, marker, &format!("{method}{marker}"));
    }
    write(path, source);
}

fn patch_delivery() {
    let path = Path::new("src/telegram/delivery.rs");
    let mut source = read(path);
    replace_if_present(
        &mut source,
        "use time::{Duration, OffsetDateTime};\n",
        "use time::{Duration, OffsetDateTime};\n\nuse crate::FrontendControlPlane;\n",
    );
    replace_if_present(
        &mut source,
        "TelegramLinkPreviewOptions, TelegramReplyParameters, TelegramSendMessage,\n        TelegramWebAppInfo,",
        "TelegramLinkPreviewOptions, TelegramOutboundFile, TelegramReplyParameters,\n        TelegramSendMessage, TelegramWebAppInfo,",
    );
    replace_if_present(
        &mut source,
        "    gateway: &mut TelegramGateway,\n    identity: &TelegramIdentity,",
        "    gateway: &mut TelegramGateway,\n    control: &FrontendControlPlane,\n    identity: &TelegramIdentity,",
    );
    replace_if_present(
        &mut source,
        "            gateway,\n            identity,",
        "            gateway,\n            control,\n            identity,",
    );
    replace_if_present(
        &mut source,
        "    gateway: &mut TelegramGateway,\n    identity: &TelegramIdentity,\n    session_id: &str,",
        "    gateway: &mut TelegramGateway,\n    control: &FrontendControlPlane,\n    identity: &TelegramIdentity,\n    session_id: &str,",
    );
    let old = r#"        TelegramAction::SendArtifact {
            artifact_id,
            evidence_ref,
            caption,
        } => {
            // Native artifact upload is handled by the attachment slice. Until a resolver provides
            // bounded bytes, preserve a safe visible reference instead of reading arbitrary paths.
            let slot = TelegramMessageSlot::Notice(format!("artifact:{artifact_id}"));
            let text = caption.as_ref().map_or_else(
                || format!("Artifact available — {evidence_ref}"),
                |caption| format!("{caption}\n\nArtifact: {evidence_ref}"),
            );
            upsert_text(
                client,
                identity,
                state,
                &slot,
                &text,
                TelegramParseMode::Plain,
                None,
                true,
            )?;
        }
"#;
    let new = r#"        TelegramAction::SendArtifact {
            artifact_id,
            evidence_ref,
            caption,
        } => {
            let artifact = control.export_attachment(artifact_id)?;
            let slot = TelegramMessageSlot::Notice(format!("artifact:{artifact_id}"));
            let reply_to_message_id = reply_target(state, &slot);
            let message = client.send_document(
                identity.chat_id,
                identity.topic_id,
                &TelegramOutboundFile {
                    file_name: artifact.display_name,
                    mime_type: artifact
                        .mime_type
                        .unwrap_or_else(|| "application/octet-stream".to_owned()),
                    bytes: artifact.bytes,
                    caption: caption
                        .clone()
                        .or_else(|| Some(format!("Evidence: {evidence_ref}"))),
                    reply_to_message_id,
                },
            )?;
            state.slots.insert(slot, message.message_id);
        }
"#;
    replace_if_present(&mut source, old, new);
    write(path, source);
}

fn patch_service() {
    let path = Path::new("src/telegram/service.rs");
    let mut source = read(path);
    replace_if_present(
        &mut source,
        "                    client,\n                    &mut self.gateway,\n                    &identity,",
        "                    client,\n                    &mut self.gateway,\n                    &self.control,\n                    &identity,",
    );
    write(path, source);
}

fn replace_if_present(source: &mut String, old: &str, new: &str) {
    if source.contains(old) {
        *source = source.replacen(old, new, 1);
    }
}

fn replace_required(source: &mut String, old: &str, new: &str) {
    let count = source.matches(old).count();
    if count != 1 {
        fail(&format!("expected one source match, found {count}: {old:?}"));
    }
    *source = source.replacen(old, new, 1);
}

fn read(path: &Path) -> String {
    match fs::read_to_string(path) {
        Ok(source) => source,
        Err(error) => fail(&format!("cannot read {}: {error}", path.display())),
    }
}

fn write(path: &Path, source: String) {
    if let Err(error) = fs::write(path, source) {
        fail(&format!("cannot write {}: {error}", path.display()));
    }
}

fn fail(message: &str) -> ! {
    eprintln!("cargo:warning={message}");
    std::process::exit(1)
}
