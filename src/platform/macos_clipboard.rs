//! macOS clipboard adapter. Tests use a uniquely named pasteboard, never general.
use crate::remote_clipboard::*;
use objc2::{MainThreadMarker, rc::Retained};
use objc2_app_kit::NSPasteboard;
use objc2_foundation::{NSArray, NSData, NSString};

pub struct MacClipboard {
    board: Retained<NSPasteboard>,
    _main_thread: MainThreadMarker,
}
impl MacClipboard {
    pub fn new(board: Retained<NSPasteboard>, main_thread: MainThreadMarker) -> Self {
        Self {
            board,
            _main_thread: main_thread,
        }
    }
    fn revision(&self) -> Revision {
        Revision(self.board.changeCount() as u64)
    }
    fn write(&self, expected: Revision, formats: &[Format]) -> Result<Revision, WriteFailure> {
        // Materialize Rust/Foundation values before the last ownership check.
        let kinds: Vec<_> = formats
            .iter()
            .map(|f| NSString::from_str(&f.name))
            .collect();
        let values: Vec<_> = formats
            .iter()
            .map(|f| NSData::with_bytes(&f.bytes))
            .collect();
        let kinds_array = NSArray::from_retained_slice(&kinds);
        if self.revision() != expected {
            return Err(WriteFailure::OwnershipLost);
        }
        // The native API has no atomic compare-and-swap operation. See remote-design.md.
        // SAFETY: No lazy owner is installed; every declared format is written below.
        let owned = Revision(unsafe { self.board.declareTypes_owner(&kinds_array, None) } as u64);
        for (kind, value) in kinds.iter().zip(values.iter()) {
            if self.revision() != owned {
                return Err(WriteFailure::OwnershipLost);
            }
            if !self.board.setData_forType(Some(value), kind) {
                return if self.revision() == owned {
                    Err(WriteFailure::Owned(owned))
                } else {
                    Err(WriteFailure::OwnershipLost)
                };
            }
        }
        if self.revision() != owned {
            return Err(WriteFailure::OwnershipLost);
        }
        Ok(owned)
    }
}
impl Clipboard for MacClipboard {
    fn snapshot(&mut self) -> Result<Snapshot, Error> {
        let revision = self.revision();
        if self
            .board
            .pasteboardItems()
            .is_some_and(|items| items.len() > 1)
        {
            return Err(Error::Unsupported);
        }
        let kinds = self.board.types().ok_or(Error::Unavailable)?;
        if kinds.iter().any(|kind| !supported(&kind.to_string())) {
            return Err(Error::Unsupported);
        }
        let mut formats = Vec::new();
        let mut total = 0usize;
        for kind in kinds.iter() {
            let data = self.board.dataForType(&kind).ok_or(Error::Unsupported)?;
            total = total.saturating_add(data.len());
            if total > 16 * 1024 * 1024 {
                return Err(Error::Unsupported);
            }
            formats.push(Format {
                name: kind.to_string(),
                bytes: data.to_vec(),
            });
        }
        if self.revision() != revision {
            return Err(Error::Changed);
        }
        Ok(Snapshot { revision, formats })
    }
    fn replace(&mut self, expected: Revision, text: &str) -> Result<Revision, WriteFailure> {
        if !crate::remote::valid_text(text) {
            return Err(WriteFailure::Unchanged(Error::InvalidText));
        }
        self.write(
            expected,
            &[Format {
                name: "public.utf8-plain-text".into(),
                bytes: text.as_bytes().to_vec(),
            }],
        )
    }
    fn restore(
        &mut self,
        expected: Revision,
        formats: &[Format],
    ) -> Result<Revision, WriteFailure> {
        if formats.iter().any(|f| !supported(&f.name)) {
            return Err(WriteFailure::Unchanged(Error::Unsupported));
        }
        self.write(expected, formats)
    }
}
fn supported(kind: &str) -> bool {
    matches!(
        kind,
        "public.utf8-plain-text"
            | "public.utf16-plain-text"
            | "public.rtf"
            | "public.html"
            | "public.png"
            | "public.tiff"
            | "public.jpeg"
            | "NSStringPboardType"
            | "NeXT Rich Text Format v1.0 pasteboard type"
            | "public.utf16-external-plain-text"
            | "CorePasteboardFlavorType 0x75743136"
    )
}
