pub use medusa_runtime::prompt::*;

pub trait ClipboardService: Send + Sync {
    fn read(&self) -> Result<ClipboardContent, ClipboardError>;
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
