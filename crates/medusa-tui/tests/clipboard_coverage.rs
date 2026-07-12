use std::path::PathBuf;

use medusa_tui::clipboard::{
    ClipboardError, ClipboardImage, ClipboardService, FileAttachment, ImageAttachment,
    PromptAttachment, PromptDraft, TextAttachment, UnsupportedClipboard, MAX_CLIPBOARD_TEXT_BYTES,
    MAX_IMAGE_BYTES, MAX_IMAGE_PIXELS, MAX_IMAGES_PER_PROMPT, MAX_TOTAL_ATTACHMENT_BYTES,
};

fn image(width: u32, height: u32) -> ClipboardImage {
    ClipboardImage {
        width,
        height,
        rgba: vec![0; width as usize * height as usize * 4],
        source_format: Some("image/rgba8".to_owned()),
    }
}

#[test]
fn clipboard_validation_rejects_zero_and_excessive_dimensions() {
    assert_eq!(
        image(0, 1).validate().expect_err("zero width"),
        ClipboardError::InvalidImageDimensions
    );
    let oversized = ClipboardImage {
        width: 40_000_001,
        height: 1,
        rgba: Vec::new(),
        source_format: None,
    };
    assert_eq!(
        oversized.validate().expect_err("pixel limit"),
        ClipboardError::ImagePixelLimit {
            pixels: 40_000_001,
            limit: MAX_IMAGE_PIXELS,
        }
    );
}

#[test]
fn clipboard_validation_rejects_image_byte_limit() {
    let pixels = MAX_IMAGE_BYTES / 4 + 1;
    let oversized = ClipboardImage {
        width: u32::try_from(pixels).expect("width"),
        height: 1,
        rgba: vec![0; pixels * 4],
        source_format: None,
    };
    assert_eq!(
        oversized.validate().expect_err("byte limit"),
        ClipboardError::ImageByteLimit {
            bytes: pixels * 4,
            limit: MAX_IMAGE_BYTES,
        }
    );
}

#[test]
fn unsupported_clipboard_returns_explicit_error() {
    let error = UnsupportedClipboard.read().expect_err("unsupported");
    assert!(matches!(error, ClipboardError::Unavailable(_)));
    assert!(error.to_string().contains("unavailable"));
}

#[test]
fn pasted_text_limits_and_cursor_boundaries_are_enforced() {
    let mut draft = PromptDraft {
        text: "é".to_owned(),
        ..PromptDraft::default()
    };
    assert_eq!(
        draft
            .insert_pasted_text(1, "x")
            .expect_err("non-boundary cursor"),
        ClipboardError::InvalidCursor(1)
    );
    assert_eq!(
        draft
            .insert_pasted_text(3, "x")
            .expect_err("cursor beyond end"),
        ClipboardError::InvalidCursor(3)
    );
    let too_large = "x".repeat(MAX_CLIPBOARD_TEXT_BYTES + 1);
    assert_eq!(
        draft
            .insert_pasted_text(draft.text.len(), &too_large)
            .expect_err("text limit"),
        ClipboardError::TextByteLimit {
            bytes: MAX_CLIPBOARD_TEXT_BYTES + 1,
            limit: MAX_CLIPBOARD_TEXT_BYTES,
        }
    );
}

#[test]
fn image_count_and_total_attachment_limits_are_enforced() {
    let mut draft = PromptDraft::default();
    for _ in 0..MAX_IMAGES_PER_PROMPT {
        draft.add_image(image(1, 1)).expect("image within count");
    }
    assert_eq!(
        draft
            .add_image(image(1, 1))
            .expect_err("image count limit"),
        ClipboardError::ImageCountLimit(MAX_IMAGES_PER_PROMPT)
    );

    let mut draft = PromptDraft {
        attachments: vec![PromptAttachment::File(FileAttachment {
            path: PathBuf::from("large.txt"),
            byte_len: MAX_TOTAL_ATTACHMENT_BYTES,
        })],
        ..PromptDraft::default()
    };
    assert_eq!(
        draft
            .add_image(image(1, 1))
            .expect_err("total attachment limit"),
        ClipboardError::TotalAttachmentByteLimit {
            bytes: MAX_TOTAL_ATTACHMENT_BYTES + 4,
            limit: MAX_TOTAL_ATTACHMENT_BYTES,
        }
    );
}

#[test]
fn attachment_byte_lengths_cover_all_variants() {
    let text = PromptAttachment::PastedText(TextAttachment {
        display_name: "paste".to_owned(),
        text: "abc".to_owned(),
    });
    let image = PromptAttachment::Image(ImageAttachment {
        display_name: "shot.png".to_owned(),
        width: 1,
        height: 1,
        rgba: vec![0; 4],
        source_format: None,
    });
    let file = PromptAttachment::File(FileAttachment {
        path: PathBuf::from("file.txt"),
        byte_len: 9,
    });
    assert_eq!(text.byte_len(), 3);
    assert_eq!(image.byte_len(), 4);
    assert_eq!(file.byte_len(), 9);
}

#[test]
fn every_clipboard_error_has_actionable_display_text() {
    let errors = [
        ClipboardError::Unavailable("offline".to_owned()),
        ClipboardError::NulByte,
        ClipboardError::InvalidCursor(7),
        ClipboardError::TextByteLimit { bytes: 8, limit: 4 },
        ClipboardError::InvalidImageDimensions,
        ClipboardError::ImagePixelLimit { pixels: 9, limit: 8 },
        ClipboardError::ImageByteCountOverflow,
        ClipboardError::InvalidRgbaLength {
            expected: 4,
            actual: 3,
        },
        ClipboardError::ImageByteLimit { bytes: 9, limit: 8 },
        ClipboardError::ImageCountLimit(2),
        ClipboardError::TotalAttachmentByteLimit { bytes: 9, limit: 8 },
    ];
    for error in errors {
        assert!(!error.to_string().trim().is_empty());
    }
}
