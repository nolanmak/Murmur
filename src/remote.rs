//! Remote paste policy. Session tokens are opaque, ephemeral adapter identities.
use std::time::Duration;

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
}
impl Controller {
    pub fn new(_sync_timeout: Duration, _dispatch_timeout: Duration) -> Self {
        Self {
            state: State::Ready,
        }
    }
    pub fn state(&self) -> State {
        self.state
    }
    pub fn begin(
        &mut self,
        _enabled: bool,
        _text: &str,
        _session: Session,
        _profile: Profile,
        _now: Duration,
    ) -> Result<Attempt, Failure> {
        Err(Failure::Disabled)
    }
    pub fn shared(&mut self, _id: Attempt, _session: Session, _now: Duration) -> bool {
        false
    }
    pub fn received(
        &mut self,
        _id: Attempt,
        _session: Session,
        _now: Duration,
    ) -> Option<Shortcut> {
        None
    }
    pub fn dispatched(&mut self, _id: Attempt, _verified: bool, _now: Duration) -> bool {
        false
    }
    pub fn observe(&mut self, _session: Session, _now: Duration) {}
    pub fn cancel(&mut self) {
        self.state = State::Cancelled;
    }
}
