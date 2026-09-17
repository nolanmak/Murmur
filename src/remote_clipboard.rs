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
pub struct Lease;
impl Lease {
    pub fn share(
        &mut self,
        _clipboard: &mut impl Clipboard,
        _id: Attempt,
        _text: &str,
    ) -> Result<(), Error> {
        Err(Error::Unavailable)
    }
    pub fn recover(
        &mut self,
        _clipboard: &mut impl Clipboard,
        _id: Attempt,
        _reason: Option<RestoreReason>,
    ) -> Result<Recovery, Error> {
        Ok(Recovery::Nothing)
    }
    pub fn pending(&self) -> bool {
        false
    }
}
