//! Attempt-scoped clipboard recovery. Payloads deliberately do not implement Debug.
use crate::remote::Attempt;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Revision(pub u64);
pub struct Format {
    pub name: String,
    pub bytes: Vec<u8>,
}
pub struct Snapshot {
    pub revision: Revision,
    /// Only fully materialized formats from a single item may be represented.
    /// Adapters reject multi-item, promised, or otherwise unsupported content.
    pub formats: Vec<Format>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Busy,
    Unsupported,
    Unavailable,
    Changed,
    WriteFailed,
    InvalidText,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WriteFailure {
    Unchanged(Error),
    /// A partial write changed the clipboard. Preserve its recovery snapshot.
    Owned(Revision),
    /// Another process took ownership during the operation. Do not overwrite it.
    OwnershipLost,
}
pub trait Clipboard {
    fn snapshot(&mut self) -> Result<Snapshot, Error>;
    /// Check revision immediately before mutation. Do not overwrite a mismatch.
    /// Return the revision of our write, never a later unrelated owner's revision.
    fn replace(&mut self, expected: Revision, text: &str) -> Result<Revision, WriteFailure>;
    fn restore(&mut self, expected: Revision, formats: &[Format])
    -> Result<Revision, WriteFailure>;
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RestoreReason {
    ConsumptionConfirmed,
    UserRequested,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Recovery {
    Nothing,
    Retained,
    Restored,
    OwnershipLost,
}
#[derive(Default)]
pub struct Lease {
    saved: Option<Saved>,
}
struct Saved {
    id: Attempt,
    owned: Revision,
    formats: Vec<Format>,
    reusable: bool,
}
impl Lease {
    pub fn share(
        &mut self,
        clipboard: &mut impl Clipboard,
        id: Attempt,
        text: &str,
    ) -> Result<(), Error> {
        if !crate::remote::valid_text(text) {
            return Err(Error::InvalidText);
        }
        let snapshot = clipboard.snapshot()?;
        if let Some(saved) = self.saved.as_ref() {
            if saved.owned == snapshot.revision && (!saved.reusable || saved.id == id) {
                return Err(Error::Busy);
            }
            // A newer clipboard owner supersedes our old remote-copy lease.
            // Do not attempt to restore over that newer content.
            if saved.owned != snapshot.revision {
                self.saved = None;
            }
        }
        match clipboard.replace(snapshot.revision, text) {
            Ok(owned) => {
                let formats = self
                    .saved
                    .take()
                    .map(|s| s.formats)
                    .unwrap_or(snapshot.formats);
                self.saved = Some(Saved {
                    id,
                    owned,
                    formats,
                    reusable: true,
                });
                Ok(())
            }
            Err(WriteFailure::Owned(owned)) => {
                let formats = self
                    .saved
                    .take()
                    .map(|s| s.formats)
                    .unwrap_or(snapshot.formats);
                self.saved = Some(Saved {
                    id,
                    owned,
                    formats,
                    reusable: false,
                });
                Err(Error::WriteFailed)
            }
            Err(WriteFailure::Unchanged(error)) => Err(error),
            Err(WriteFailure::OwnershipLost) => Err(Error::Changed),
        }
    }
    pub fn recover(
        &mut self,
        clipboard: &mut impl Clipboard,
        id: Attempt,
        reason: Option<RestoreReason>,
    ) -> Result<Recovery, Error> {
        let Some(saved) = self.saved.as_mut().filter(|s| s.id == id) else {
            return Ok(Recovery::Nothing);
        };
        if reason.is_none() {
            return Ok(Recovery::Retained);
        }
        saved.reusable = false;
        match clipboard.restore(saved.owned, &saved.formats) {
            Ok(_) => {
                self.saved = None;
                Ok(Recovery::Restored)
            }
            Err(WriteFailure::OwnershipLost) => {
                self.saved = None;
                Ok(Recovery::OwnershipLost)
            }
            Err(WriteFailure::Unchanged(error)) => Err(error),
            Err(WriteFailure::Owned(owned)) => {
                saved.owned = owned;
                Err(Error::WriteFailed)
            }
        }
    }
    pub fn pending(&self) -> bool {
        self.saved.is_some()
    }
}
