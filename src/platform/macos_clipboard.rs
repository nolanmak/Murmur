//! macOS clipboard adapter. Tests use a uniquely named pasteboard, never general.
use crate::remote_clipboard::*;
use objc2::rc::Retained;
use objc2_app_kit::NSPasteboard;

pub struct MacClipboard {
    _board: Retained<NSPasteboard>,
}
impl MacClipboard {
    pub fn new(board: Retained<NSPasteboard>) -> Self {
        Self { _board: board }
    }
}
impl Clipboard for MacClipboard {
    fn snapshot(&mut self) -> Result<Snapshot, Error> {
        Err(Error::Unavailable)
    }
    fn replace(&mut self, _expected: Revision, _text: &str) -> Result<Revision, WriteFailure> {
        Err(WriteFailure::Unchanged(Error::Unavailable))
    }
    fn restore(
        &mut self,
        _expected: Revision,
        _formats: &[Format],
    ) -> Result<Revision, WriteFailure> {
        Err(WriteFailure::Unchanged(Error::Unavailable))
    }
}
