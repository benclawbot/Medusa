//! Canonical frontend-neutral prompt attachment model.
//!
//! Frontends should ingest clipboard, picker, drag-and-drop, or screenshot data into these
//! types before handing a prompt to the runtime. Keeping validation and limits here prevents
//! desktop and terminal clients from developing incompatible attachment behavior.
//! Durable session ownership and journal replay live in [`session`].

pub mod session;

use std::{fmt, path::PathBuf};

use serde::{Deserialize, Serialize};

pub const MAX_CLIPBOARD_TEXT_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_IMAGE_BYTES: usize = 20 * 1024 * 1024;
pub const MAX_IMAGE_PIXELS: u64 = 40_000_000;
pub const MAX_IMAGES_PER_PROMPT: usize = 10;
pub const MAX_TOTAL_ATTACHMENT_BYTES: usize = 50 * 1024 * 1024;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum AttachmentOrigin {
    Clipboard,
    FilePicker,
    DragAndDrop,
    Screenshot,
    ClipboardFileList,
    Unknown,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum ClipboardContent {
    Empty,
    Text(String),
    Image(ClipboardImage),
    Files(Vec<PathBuf>),
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ClipboardImage {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    pub source_format: Option<String>,
}

impl ClipboardImage {
    pub fn validate(&self) -> Result<(), ClipboardError> {
        validate_rgba_image(self.width, self.height, &self.rgba)
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct PromptDraft {
    pub text: String,
    pub attachments: Vec<PromptAttachment>,
    pub revision: u64,
}

impl PromptDraft {
    pub fn insert_pasted_text(
        &mut self,
        cursor: usize,
        pasted: &str,
    ) -> Result<(), ClipboardError> {
        if pasted.as_bytes().contains(&0) {
            return Err(ClipboardError::NulByte);
        }
        if pasted.len() > MAX_CLIPBOARD_TEXT_BYTES {
            return Err(ClipboardError::TextByteLimit {
                bytes: pasted.len(),
                limit: MAX_CLIPBOARD_TEXT_BYTES,
            });
        }
        if cursor > self.text.len() || !self.text.is_char_boundary(cursor) {
            return Err(ClipboardError::InvalidCursor(cursor));
        }
        let normalized = normalize_line_endings(pasted);
        self.text.insert_str(cursor, &normalized);
        self.revision = self.revision.saturating_add(1);
        Ok(())
    }

    pub fn add_image(&mut self, image: ClipboardImage) -> Result<(), ClipboardError> {
        image.validate()?;
        let image_count = self
            .attachments
            .iter()
            .filter(|attachment| matches!(attachment, PromptAttachment::Image(_)))
            .count();
        if image_count >= MAX_IMAGES_PER_PROMPT {
            return Err(ClipboardError::ImageCountLimit(MAX_IMAGES_PER_PROMPT));
        }
        let total = self
            .total_attachment_bytes()
            .saturating_add(image.rgba.len());
        if total > MAX_TOTAL_ATTACHMENT_BYTES {
            return Err(ClipboardError::TotalAttachmentByteLimit {
                bytes: total,
                limit: MAX_TOTAL_ATTACHMENT_BYTES,
            });
        }
        self.attachments
            .push(PromptAttachment::Image(ImageAttachment {
                display_name: format!("screenshot-{}.png", image_count + 1),
                width: image.width,
                height: image.height,
                rgba: image.rgba,
                source_format: image.source_format,
            }));
        self.revision = self.revision.saturating_add(1);
        Ok(())
    }

    pub fn total_attachment_bytes(&self) -> usize {
        self.attachments
            .iter()
            .map(PromptAttachment::byte_len)
            .sum()
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum PromptAttachment {
    PastedText(TextAttachment),
    Image(ImageAttachment),
    File(FileAttachment),
}

impl PromptAttachment {
    pub fn byte_len(&self) -> usize {
        match self {
            Self::PastedText(attachment) => attachment.text.len(),
            Self::Image(attachment) => attachment.rgba.len(),
            Self::File(attachment) => attachment.byte_len,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TextAttachment {
    pub display_name: String,
    pub text: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ImageAttachment {
    pub display_name: String,
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    pub source_format: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FileAttachment {
    pub path: PathBuf,
    pub byte_len: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ClipboardError {
    Unavailable(String),
    NulByte,
    InvalidCursor(usize),
    TextByteLimit { bytes: usize, limit: usize },
    InvalidImageDimensions,
    ImagePixelLimit { pixels: u64, limit: u64 },
    ImageByteCountOverflow,
    InvalidRgbaLength { expected: u64, actual: usize },
    ImageByteLimit { bytes: usize, limit: usize },
    ImageCountLimit(usize),
    TotalAttachmentByteLimit { bytes: usize, limit: usize },
}

impl fmt::Display for ClipboardError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unavailable(message) => formatter.write_str(message),
            Self::NulByte => formatter.write_str("clipboard text contains a NUL byte"),
            Self::InvalidCursor(cursor) => write!(formatter, "invalid paste cursor {cursor}"),
            Self::TextByteLimit { bytes, limit } => {
                write!(
                    formatter,
                    "clipboard text is {bytes} bytes; limit is {limit}"
                )
            }
            Self::InvalidImageDimensions => {
                formatter.write_str("clipboard image has zero dimensions")
            }
            Self::ImagePixelLimit { pixels, limit } => {
                write!(
                    formatter,
                    "clipboard image has {pixels} pixels; limit is {limit}"
                )
            }
            Self::ImageByteCountOverflow => {
                formatter.write_str("clipboard image byte count overflowed")
            }
            Self::InvalidRgbaLength { expected, actual } => write!(
                formatter,
                "clipboard image RGBA length is {actual}; expected {expected}"
            ),
            Self::ImageByteLimit { bytes, limit } => {
                write!(
                    formatter,
                    "clipboard image is {bytes} bytes; limit is {limit}"
                )
            }
            Self::ImageCountLimit(limit) => {
                write!(formatter, "prompt allows at most {limit} images")
            }
            Self::TotalAttachmentByteLimit { bytes, limit } => write!(
                formatter,
                "prompt attachments total {bytes} bytes; limit is {limit}"
            ),
        }
    }
}

impl std::error::Error for ClipboardError {}

fn validate_rgba_image(width: u32, height: u32, rgba: &[u8]) -> Result<(), ClipboardError> {
    let pixels = u64::from(width) * u64::from(height);
    if width == 0 || height == 0 {
        return Err(ClipboardError::InvalidImageDimensions);
    }
    if pixels > MAX_IMAGE_PIXELS {
        return Err(ClipboardError::ImagePixelLimit {
            pixels,
            limit: MAX_IMAGE_PIXELS,
        });
    }
    let expected = pixels
        .checked_mul(4)
        .ok_or(ClipboardError::ImageByteCountOverflow)?;
    if usize::try_from(expected).ok() != Some(rgba.len()) {
        return Err(ClipboardError::InvalidRgbaLength {
            expected,
            actual: rgba.len(),
        });
    }
    if rgba.len() > MAX_IMAGE_BYTES {
        return Err(ClipboardError::ImageByteLimit {
            bytes: rgba.len(),
            limit: MAX_IMAGE_BYTES,
        });
    }
    Ok(())
}

fn normalize_line_endings(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multiline_paste_is_inserted_as_inert_text() {
        let mut draft = PromptDraft {
            text: "before after".to_owned(),
            ..PromptDraft::default()
        };
        draft
            .insert_pasted_text(7, "echo unsafe\r\nsecond line\rthird")
            .expect("paste text");
        assert_eq!(draft.text, "before echo unsafe\nsecond line\nthirdafter");
        assert_eq!(draft.revision, 1);
    }

    #[test]
    fn valid_rgba_screenshot_becomes_attachment() {
        let mut draft = PromptDraft::default();
        draft
            .add_image(ClipboardImage {
                width: 2,
                height: 1,
                rgba: vec![0; 8],
                source_format: Some("image/png".to_owned()),
            })
            .expect("attach image");
        assert_eq!(draft.attachments.len(), 1);
        assert_eq!(draft.total_attachment_bytes(), 8);
    }

    #[test]
    fn malformed_rgba_length_is_rejected() {
        let error = ClipboardImage {
            width: 2,
            height: 2,
            rgba: vec![0; 15],
            source_format: None,
        }
        .validate()
        .expect_err("reject malformed image");
        assert_eq!(
            error,
            ClipboardError::InvalidRgbaLength {
                expected: 16,
                actual: 15,
            }
        );
    }
}
