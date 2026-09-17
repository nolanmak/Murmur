//! Remote paste policy. Session tokens are opaque, ephemeral adapter identities.
use std::time::Duration;

pub(crate) fn valid_text(text: &str) -> bool {
    !text.trim().is_empty()
        && !text
            .chars()
            .any(|c| c.is_control() || matches!(c, '\u{2028}' | '\u{2029}'))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Destination {
    pub session: u64,
    pub connection_generation: u64,
    pub window: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Session {
    pub destination: Destination,
    pub foreground: bool,
    pub connected: bool,
    pub unlocked: bool,
    pub isolated: bool,
    pub clipboard_enabled: bool,
    pub keyboard_enabled: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Profile {
    Mac,
    Linux,
    LinuxTerminal,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Shortcut {
    pub command: bool,
    pub control: bool,
    pub shift: bool,
    pub key: char,
}
impl Profile {
    pub fn shortcut(self) -> Shortcut {
        Shortcut {
            command: self == Self::Mac,
            control: self != Self::Mac,
            shift: self == Self::LinuxTerminal,
            key: 'v',
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Failure {
    Disabled,
    Busy,
    InvalidText,
    DestinationChanged,
    Disconnected,
    Locked,
    Ambiguous,
    ClipboardDisabled,
    KeyboardDenied,
    TimedOut,
    ClipboardWriteFailed,
    DispatchFailed,
    DispatchUncertain,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Ready,
    Preparing,
    WaitingForClipboard,
    PasteRequested,
    PasteSent,
    Inserted,
    Failed(Failure),
    Cancelled,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Attempt(pub u64);
pub struct Controller {
    state: State,
    generation: u64,
    pending: Option<Pending>,
    sync_timeout: Duration,
    dispatch_timeout: Duration,
}
struct Pending {
    id: Attempt,
    destination: Destination,
    profile: Profile,
    deadline: Duration,
}
impl Session {
    fn validate(self) -> Result<(), Failure> {
        if !self.connected {
            return Err(Failure::Disconnected);
        }
        if !self.unlocked {
            return Err(Failure::Locked);
        }
        if !self.foreground {
            return Err(Failure::DestinationChanged);
        }
        if !self.isolated {
            return Err(Failure::Ambiguous);
        }
        if !self.clipboard_enabled {
            return Err(Failure::ClipboardDisabled);
        }
        if !self.keyboard_enabled {
            return Err(Failure::KeyboardDenied);
        }
        Ok(())
    }
}
impl Controller {
    pub fn new(sync_timeout: Duration, dispatch_timeout: Duration) -> Self {
        Self {
            state: State::Ready,
            generation: 0,
            pending: None,
            sync_timeout,
            dispatch_timeout,
        }
    }
    pub fn state(&self) -> State {
        self.state
    }
    pub fn begin(
        &mut self,
        enabled: bool,
        text: &str,
        session: Session,
        profile: Profile,
        now: Duration,
    ) -> Result<Attempt, Failure> {
        if self.pending.is_some() {
            return Err(Failure::Busy);
        }
        let validation = if !enabled {
            Err(Failure::Disabled)
        } else if !valid_text(text) {
            Err(Failure::InvalidText)
        } else {
            session.validate()
        };
        if let Err(failure) = validation {
            self.state = State::Failed(failure);
            return Err(failure);
        }
        self.generation = self
            .generation
            .checked_add(1)
            .expect("attempt counter exhausted");
        let id = Attempt(self.generation);
        self.pending = Some(Pending {
            id,
            destination: session.destination,
            profile,
            deadline: now.saturating_add(self.sync_timeout),
        });
        self.state = State::Preparing;
        Ok(id)
    }
    /// Call immediately before replacing the clipboard, with a fresh session snapshot.
    /// A false return prohibits sharing. The adapter reports a write failure separately.
    pub fn shared(&mut self, id: Attempt, session: Session, now: Duration) -> bool {
        if !self.matches(id, State::Preparing) {
            return false;
        }
        self.observe(session, now);
        if self.pending.is_none() {
            return false;
        }
        self.state = State::WaitingForClipboard;
        true
    }
    pub fn received(&mut self, id: Attempt, session: Session, now: Duration) -> Option<Shortcut> {
        if !self.matches(id, State::WaitingForClipboard) {
            return None;
        }
        self.observe(session, now);
        let pending = self.pending.as_mut()?;
        pending.deadline = now.saturating_add(self.dispatch_timeout);
        self.state = State::PasteRequested;
        Some(pending.profile.shortcut())
    }
    pub fn dispatched(&mut self, id: Attempt, verified: bool, now: Duration) -> bool {
        if !self.matches(id, State::PasteRequested) {
            return false;
        }
        if self.pending.as_ref().is_some_and(|p| now >= p.deadline) {
            self.fail(Failure::TimedOut);
            return false;
        }
        self.pending = None;
        self.state = if verified {
            State::Inserted
        } else {
            State::PasteSent
        };
        true
    }
    pub fn observe(&mut self, session: Session, now: Duration) {
        let Some(pending) = self.pending.as_ref() else {
            return;
        };
        let failure = session
            .validate()
            .err()
            .or_else(|| {
                (session.destination != pending.destination).then_some(Failure::DestinationChanged)
            })
            .or_else(|| (now >= pending.deadline).then_some(Failure::TimedOut));
        if let Some(failure) = failure {
            self.fail(failure);
        }
    }
    fn matches(&self, id: Attempt, state: State) -> bool {
        self.state == state && self.pending.as_ref().is_some_and(|p| p.id == id)
    }
    fn fail(&mut self, failure: Failure) {
        self.pending = None;
        self.state = State::Failed(failure);
    }
    /// Report an adapter failure for the active attempt only. An uncertain
    /// dispatch is terminal: it must never trigger an automatic retry.
    pub fn adapter_failed(&mut self, id: Attempt, failure: Failure) -> bool {
        if !self
            .pending
            .as_ref()
            .is_some_and(|pending| pending.id == id)
        {
            return false;
        }
        self.fail(failure);
        true
    }
    pub fn cancel(&mut self) {
        self.pending = None;
        self.state = State::Cancelled;
    }
}
