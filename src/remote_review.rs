//! Explicit review policy for the manual RustDesk fallback. No payload logging.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Window {
    pub process: i32,
    pub element: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReviewId(pub u64);
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    InvalidText,
    NoWindow,
    ChangedWindow,
    ConfirmationNeeded,
    Stale,
}
#[derive(Default)]
pub struct Review;
impl Review {
    pub fn prepare(&mut self, _text: &str, _window: Option<Window>) -> Result<ReviewId, Error> {
        Err(Error::NoWindow)
    }
    pub fn confirm(
        &mut self,
        _id: ReviewId,
        _window: Option<Window>,
        _confirmed: bool,
    ) -> Result<String, Error> {
        Err(Error::Stale)
    }
    pub fn cancel(&mut self) {}
}
