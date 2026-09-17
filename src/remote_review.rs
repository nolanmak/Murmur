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
pub struct Review {
    generation: u64,
    pending: Option<(ReviewId, Window, String)>,
}
impl Review {
    pub fn prepare(&mut self, text: &str, window: Option<Window>) -> Result<ReviewId, Error> {
        self.cancel();
        if !crate::remote::valid_text(text) {
            return Err(Error::InvalidText);
        }
        let window = window.ok_or(Error::NoWindow)?;
        self.generation = self
            .generation
            .checked_add(1)
            .expect("review counter exhausted");
        let id = ReviewId(self.generation);
        self.pending = Some((id, window, text.into()));
        Ok(id)
    }
    pub fn confirm(
        &mut self,
        id: ReviewId,
        window: Option<Window>,
        confirmed: bool,
    ) -> Result<String, Error> {
        let Some((pending, selected, _)) = &self.pending else {
            return Err(Error::Stale);
        };
        if *pending != id {
            return Err(Error::Stale);
        }
        if Some(*selected) != window {
            self.cancel();
            return Err(Error::ChangedWindow);
        }
        if !confirmed {
            return Err(Error::ConfirmationNeeded);
        }
        Ok(self.pending.take().expect("validated review").2)
    }
    pub fn cancel(&mut self) {
        self.pending = None;
    }
}
