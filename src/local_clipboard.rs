//! Local paste transaction. The clipboard remains ours until explicit recovery.
//! A dispatched shortcut cannot prove that another application consumed it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Revision(pub u64);
#[derive(Clone, PartialEq, Eq)]
pub struct Format {
    pub kind: String,
    pub data: Vec<u8>,
}
#[derive(Clone, PartialEq, Eq)]
pub struct Item {
    pub formats: Vec<Format>,
}
#[derive(Clone)]
pub struct Snapshot {
    pub revision: Revision,
    pub items: Vec<Item>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Failure {
    PendingRestore,
    UnsupportedClipboard,
    Changed,
    WriteFailed,
    DispatchFailed,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WriteFailure {
    Changed,
    Unchanged,
    Owned(Revision),
}
pub trait Board {
    fn snapshot(&mut self) -> Result<Snapshot, Failure>;
    fn replace(&mut self, expected: Revision, items: &[Item]) -> Result<Revision, WriteFailure>;
    fn dispatch_paste(&mut self) -> bool;
}
const MAX_ITEMS: usize = 32;
const MAX_FORMATS_PER_ITEM: usize = 32;
const MAX_ITEM_BYTES: usize = 8 * 1024 * 1024;
const MAX_BYTES: usize = 16 * 1024 * 1024;
fn within_bounds(items: &[Item]) -> bool {
    if items.len() > MAX_ITEMS {
        return false;
    }
    let mut total = 0usize;
    for item in items {
        let mut item_bytes = 0usize;
        if item.formats.is_empty() || item.formats.len() > MAX_FORMATS_PER_ITEM {
            return false;
        }
        for format in &item.formats {
            if format.kind.is_empty() || format.kind.len() > 256 {
                return false;
            }
            item_bytes = item_bytes.saturating_add(format.data.len());
            total = total.saturating_add(format.data.len());
            if item_bytes > MAX_ITEM_BYTES || total > MAX_BYTES {
                return false;
            }
        }
    }
    true
}
#[derive(Default)]
pub struct Lease {
    saved: Option<(Revision, Vec<Item>)>,
    reusable: bool,
}
impl Lease {
    pub fn pending(&self) -> bool {
        self.saved.is_some()
    }
    pub fn send(&mut self, board: &mut impl Board, text: &str) -> Result<(), Failure> {
        let before = board.snapshot()?;
        if let Some((owned, _)) = self.saved.as_ref() {
            if *owned == before.revision && !self.reusable {
                return Err(Failure::PendingRestore);
            }
            // The clipboard has a newer owner. Its contents must win, so the
            // old restoration lease is no longer actionable and can be
            // released before starting this new paste.
            if *owned != before.revision {
                self.saved = None;
            }
        }
        if !within_bounds(&before.items) {
            return Err(Failure::UnsupportedClipboard);
        }
        let replacement = [Item {
            formats: vec![Format {
                kind: "public.utf8-plain-text".into(),
                data: text.as_bytes().to_vec(),
            }],
        }];
        match board.replace(before.revision, &replacement) {
            Ok(owned) => {
                let original = self
                    .saved
                    .take()
                    .map(|(_, items)| items)
                    .unwrap_or(before.items);
                self.saved = Some((owned, original));
            }
            Err(WriteFailure::Owned(owned)) => {
                let original = self
                    .saved
                    .take()
                    .map(|(_, items)| items)
                    .unwrap_or(before.items);
                self.saved = Some((owned, original));
                self.reusable = false;
                return Err(Failure::WriteFailed);
            }
            Err(WriteFailure::Changed) => return Err(Failure::Changed),
            Err(WriteFailure::Unchanged) => return Err(Failure::WriteFailed),
        }
        self.reusable = board.dispatch_paste();
        if !self.reusable {
            return Err(Failure::DispatchFailed);
        }
        Ok(())
    }
    pub fn restore(&mut self, board: &mut impl Board) -> Result<(), Failure> {
        self.reusable = false;
        let (owned, saved) = self.saved.as_ref().ok_or(Failure::PendingRestore)?;
        match board.replace(*owned, saved) {
            Ok(_) => {
                self.saved = None;
                Ok(())
            }
            Err(WriteFailure::Unchanged) => Err(Failure::WriteFailed),
            Err(WriteFailure::Changed) => {
                self.saved = None; // A newer clipboard owner wins.
                Err(Failure::Changed)
            }
            Err(WriteFailure::Owned(revision)) => {
                self.saved.as_mut().unwrap().0 = revision;
                Err(Failure::WriteFailed)
            }
        }
    }
    pub fn abandon(&mut self) {
        self.saved = None;
    }
}
