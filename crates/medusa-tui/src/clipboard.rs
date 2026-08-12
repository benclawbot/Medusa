use std::path::Path;

pub use medusa_runtime::prompt::*;

pub trait ClipboardService: Send + Sync {
    fn read(&self) -> Result<ClipboardContent, ClipboardError>;

    fn write_text(&self, _text: &str) -> Result<(), ClipboardError> {
        Err(ClipboardError::Unavailable(
            "clipboard write is unavailable in this build".to_owned(),
        ))
    }
}

#[derive(Default)]
pub struct UnsupportedClipboard;

impl ClipboardService for UnsupportedClipboard {
    fn read(&self) -> Result<ClipboardContent, ClipboardError> {
        Err(ClipboardError::Unavailable(
            "clipboard access is unavailable in this build".to_owned(),
        ))
    }
}

/// Decode an image file into the canonical shared prompt attachment representation.
///
/// The file is decoded to RGBA8 and then passed through `PromptDraft::add_image`, so
/// file selection and clipboard images share the exact same size, dimension, count,
/// and total-payload validation.
pub fn attach_image_file(draft: &mut PromptDraft, path: &Path) -> Result<(), ClipboardError> {
    let reader = image::ImageReader::open(path).map_err(|error| {
        ClipboardError::Unavailable(format!("could not open image {}: {error}", path.display()))
    })?;
    let reader = reader.with_guessed_format().map_err(|error| {
        ClipboardError::Unavailable(format!(
            "could not determine image format for {}: {error}",
            path.display()
        ))
    })?;
    let source_format = reader.format().map(|format| {
        format!(
            "image/{}",
            format
                .extensions_str()
                .first()
                .copied()
                .unwrap_or("unknown")
        )
    });
    let decoded = reader.decode().map_err(|error| {
        ClipboardError::Unavailable(format!(
            "could not decode image {}: {error}",
            path.display()
        ))
    })?;
    let width = decoded.width();
    let height = decoded.height();
    let rgba = decoded.into_rgba8().into_raw();
    draft.add_image(ClipboardImage {
        width,
        height,
        rgba,
        source_format,
    })?;
    if let Some(PromptAttachment::Image(image)) = draft.attachments.last_mut()
        && let Some(name) = path.file_name().and_then(|name| name.to_str())
    {
        image.display_name = name.to_owned();
    }
    Ok(())
}

/// Remove an attachment by its zero-based index and advance the draft revision.
pub fn remove_attachment(draft: &mut PromptDraft, index: usize) -> Option<PromptAttachment> {
    if index >= draft.attachments.len() {
        return None;
    }
    let removed = draft.attachments.remove(index);
    draft.revision = draft.revision.saturating_add(1);
    Some(removed)
}

/// Produce concise metadata suitable for the terminal composer/status area.
#[must_use]
pub fn attachment_summary(draft: &PromptDraft) -> String {
    let image_count = draft
        .attachments
        .iter()
        .filter(|attachment| matches!(attachment, PromptAttachment::Image(_)))
        .count();
    let total_bytes = draft.total_attachment_bytes();
    match image_count {
        0 => "no images attached".to_owned(),
        1 => {
            let dimensions = draft
                .attachments
                .iter()
                .find_map(|attachment| match attachment {
                    PromptAttachment::Image(image) => {
                        Some(format!("{}×{}", image.width, image.height))
                    }
                    _ => None,
                });
            format!(
                "1 image · {} · {}",
                dimensions.unwrap_or_else(|| "unknown dimensions".to_owned()),
                human_bytes(total_bytes)
            )
        }
        count => format!("{count} images · {}", human_bytes(total_bytes)),
    }
}

fn human_bytes(bytes: usize) -> String {
    const KIB: usize = 1024;
    const MIB: usize = 1024 * KIB;
    if bytes >= MIB {
        format!("{:.1} MiB", bytes as f64 / MIB as f64)
    } else if bytes >= KIB {
        format!("{:.1} KiB", bytes as f64 / KIB as f64)
    } else {
        format!("{bytes} B")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selected_image_file_uses_shared_validation_and_preserves_name() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("selected.png");
        let image = image::RgbaImage::from_raw(2, 1, vec![255; 8]).expect("rgba fixture");
        image.save(&path).expect("save fixture");

        let mut draft = PromptDraft::default();
        attach_image_file(&mut draft, &path).expect("attach selected image");

        assert_eq!(draft.attachments.len(), 1);
        let PromptAttachment::Image(image) = &draft.attachments[0] else {
            panic!("expected image attachment");
        };
        assert_eq!(image.display_name, "selected.png");
        assert_eq!((image.width, image.height), (2, 1));
        assert_eq!(image.rgba.len(), 8);
    }

    #[test]
    fn removal_updates_revision_and_keeps_remaining_attachments() {
        let mut draft = PromptDraft::default();
        for value in [1_u8, 2_u8] {
            draft
                .add_image(ClipboardImage {
                    width: 1,
                    height: 1,
                    rgba: vec![value; 4],
                    source_format: Some("image/rgba8".to_owned()),
                })
                .expect("attach image");
        }
        let revision = draft.revision;
        let removed = remove_attachment(&mut draft, 0).expect("remove first attachment");
        assert!(matches!(removed, PromptAttachment::Image(_)));
        assert_eq!(draft.attachments.len(), 1);
        assert_eq!(draft.revision, revision + 1);
    }

    #[test]
    fn summary_is_concise_and_includes_dimensions_and_size() {
        let mut draft = PromptDraft::default();
        draft
            .add_image(ClipboardImage {
                width: 10,
                height: 20,
                rgba: vec![0; 800],
                source_format: Some("image/rgba8".to_owned()),
            })
            .expect("attach image");
        assert_eq!(attachment_summary(&draft), "1 image · 10×20 · 800 B");
    }
}
