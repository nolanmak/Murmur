//! Materialized, item-preserving adapter for local paste only. Remote review keeps
//! its separate single-item policy. Native clipboard writes cannot be atomic.
use crate::local_clipboard::{Board, Failure, Format, Item, Revision, Snapshot, WriteFailure};
use crate::local_delivery::{CopyBoard, CopyFailure};
use objc2::{MainThreadMarker, rc::Retained, runtime::ProtocolObject};
use objc2_app_kit::{NSPasteboard, NSPasteboardItem, NSPasteboardWriting};
use objc2_foundation::{NSArray, NSData, NSString};

pub struct MacLocalBoard<'a> {
    board: Retained<NSPasteboard>,
    _main: MainThreadMarker,
    dispatch: Box<dyn FnMut() -> bool + 'a>,
}
impl<'a> MacLocalBoard<'a> {
    pub fn new(
        board: Retained<NSPasteboard>,
        main: MainThreadMarker,
        dispatch: impl FnMut() -> bool + 'a,
    ) -> Self {
        Self {
            board,
            _main: main,
            dispatch: Box::new(dispatch),
        }
    }
    fn revision(&self) -> Revision {
        Revision(self.board.changeCount() as u64)
    }
}
impl Board for MacLocalBoard<'_> {
    fn snapshot(&mut self) -> Result<Snapshot, Failure> {
        let revision = self.revision();
        let mut saved = Vec::new();
        if let Some(items) = self.board.pasteboardItems() {
            if items.len() > 32 {
                return Err(Failure::UnsupportedClipboard);
            }
            let mut total = 0usize;
            for item in items.iter() {
                let types = item.types();
                if types.is_empty() || types.len() > 32 {
                    return Err(Failure::UnsupportedClipboard);
                }
                let mut formats = Vec::new();
                let mut item_bytes = 0usize;
                for kind in types.iter() {
                    let name = kind.to_string();
                    if name.len() > 256 || name.to_lowercase().contains("promise") {
                        return Err(Failure::UnsupportedClipboard);
                    }
                    let data = item
                        .dataForType(&kind)
                        .ok_or(Failure::UnsupportedClipboard)?;
                    item_bytes = item_bytes.saturating_add(data.len());
                    total = total.saturating_add(data.len());
                    if item_bytes > 8 * 1024 * 1024 || total > 16 * 1024 * 1024 {
                        return Err(Failure::UnsupportedClipboard);
                    }
                    formats.push(Format {
                        kind: name,
                        data: data.to_vec(),
                    });
                }
                saved.push(Item { formats });
            }
        } else if self.board.types().is_some_and(|types| !types.is_empty()) {
            return Err(Failure::UnsupportedClipboard);
        }
        if self.revision() != revision {
            return Err(Failure::Changed);
        }
        Ok(Snapshot {
            revision,
            items: saved,
        })
    }
    fn replace(&mut self, expected: Revision, items: &[Item]) -> Result<Revision, WriteFailure> {
        // Build all native objects before the last ownership check.
        let mut objects = Vec::new();
        for item in items {
            let native = NSPasteboardItem::new();
            for format in &item.formats {
                if !native.setData_forType(
                    &NSData::with_bytes(&format.data),
                    &NSString::from_str(&format.kind),
                ) {
                    return Err(WriteFailure::Unchanged);
                }
            }
            objects.push(native);
        }
        if self.revision() != expected {
            return Err(WriteFailure::Changed);
        }
        let owned = Revision(self.board.clearContents() as u64);
        if objects.is_empty() {
            return Ok(owned);
        }
        if self.revision() != owned {
            return Err(WriteFailure::Changed);
        }
        let refs: Vec<&ProtocolObject<dyn NSPasteboardWriting>> = objects
            .iter()
            .map(|item| ProtocolObject::from_ref(&**item))
            .collect();
        let success = self.board.writeObjects(&NSArray::from_slice(&refs));
        let after = self.revision();
        if !success {
            return Err(WriteFailure::Owned(after));
        }
        Ok(after)
    }
    fn dispatch_paste(&mut self) -> bool {
        (self.dispatch)()
    }
}

impl CopyBoard for MacLocalBoard<'_> {
    fn overwrite_with_text(&mut self, text: &str) -> Result<(), CopyFailure> {
        self.board.clearContents();
        if self.board.setString_forType(
            &NSString::from_str(text),
            &NSString::from_str("public.utf8-plain-text"),
        ) {
            Ok(())
        } else {
            Err(CopyFailure::WriteFailedAfterClear)
        }
    }
}
