use std::sync::Mutex;

use arboard::Clipboard;

use crate::clipboard::{ClipboardContent, ClipboardError, ClipboardImage, ClipboardService};

/// Cross-platform clipboard implementation backed by the operating system.
///
/// Operations are serialized because Windows exposes a process-global clipboard
/// lock and parallel reads are inherently racy on all supported platforms.
pub struct NativeClipboard {
    clipboard: Mutex<Clipboard>,
}

impl NativeClipboard {
    pub fn new() -> Result<Self, ClipboardError> {
        let clipboard = Clipboard::new().map_err(|error| {
            ClipboardError::Unavailable(format!("failed to initialize clipboard: {error}"))
        })?;
        Ok(Self {
            clipboard: Mutex::new(clipboard),
        })
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, Clipboard>, ClipboardError> {
        self.clipboard
            .lock()
            .map_err(|_| ClipboardError::Unavailable("clipboard lock was poisoned".to_owned()))
    }
}

impl ClipboardService for NativeClipboard {
    fn read(&self) -> Result<ClipboardContent, ClipboardError> {
        let mut clipboard = self.lock()?;

        match clipboard.get_image() {
            Ok(image) => {
                let width = u32::try_from(image.width).map_err(|_| {
                    ClipboardError::Unavailable(
                        "clipboard image width exceeds supported range".to_owned(),
                    )
                })?;
                let height = u32::try_from(image.height).map_err(|_| {
                    ClipboardError::Unavailable(
                        "clipboard image height exceeds supported range".to_owned(),
                    )
                })?;
                let image = ClipboardImage {
                    width,
                    height,
                    rgba: image.bytes.into_owned(),
                    source_format: Some("image/rgba8".to_owned()),
                };
                image.validate()?;
                return Ok(ClipboardContent::Image(image));
            }
            Err(image_error) => match clipboard.get_text() {
                Ok(text) if text.is_empty() => Ok(ClipboardContent::Empty),
                Ok(text) => Ok(ClipboardContent::Text(text)),
                Err(text_error) => Err(ClipboardError::Unavailable(format!(
                    "clipboard contains neither a supported image nor UTF-8 text: image={image_error}; text={text_error}"
                ))),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_clipboard_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<NativeClipboard>();
    }
}
