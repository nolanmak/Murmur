//! Hold-to-dictate preserves quick spaces and typing rollover.
pub const HOLD_MS: u64 = 350;
#[derive(Clone, Copy)]
pub enum Key {
    Down,
    ModifiedDown,
    Repeat,
    Up,
    Other,
    Escape,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    Start,
    Finish,
    Cancel,
}
#[derive(Default)]
pub struct Action {
    pub consume: bool,
    pub replay: bool,
    pub command: Option<Command>,
}
#[derive(Default)]
enum State {
    #[default]
    Idle,
    Pending(u64),
    Dictating,
    Cancelled,
    PassThrough,
}
#[derive(Default)]
pub struct HoldSpace {
    state: State,
}
impl HoldSpace {
    pub fn key(&mut self, key: Key, now: u64) -> Action {
        use State::*;
        match key {
            Key::Down => {
                if matches!(self.state, Idle) {
                    self.state = Pending(now);
                }
                Action {
                    consume: !matches!(self.state, PassThrough),
                    ..Action::default()
                }
            }
            Key::ModifiedDown => {
                self.state = PassThrough;
                Action::default()
            }
            Key::Repeat => Action {
                consume: matches!(self.state, Pending(_) | Dictating | Cancelled),
                ..Action::default()
            },
            Key::Up => {
                let previous = std::mem::take(&mut self.state);
                match previous {
                    Pending(_) => Action {
                        replay: true,
                        ..Action::default()
                    },
                    Dictating | Cancelled => Action {
                        consume: true,
                        command: Some(Command::Finish),
                        ..Action::default()
                    },
                    _ => Action::default(),
                }
            }
            Key::Other | Key::Escape => match self.state {
                Pending(_) => {
                    self.state = PassThrough;
                    Action {
                        replay: true,
                        ..Action::default()
                    }
                }
                Dictating => {
                    self.state = Cancelled;
                    Action {
                        consume: matches!(key, Key::Escape),
                        command: Some(Command::Cancel),
                        ..Action::default()
                    }
                }
                _ => Action {
                    command: matches!(key, Key::Escape).then_some(Command::Cancel),
                    ..Action::default()
                },
            },
        }
    }
    pub fn tick(&mut self, now: u64) -> Action {
        if let State::Pending(since) = self.state
            && now.saturating_sub(since) >= HOLD_MS
        {
            self.state = State::Dictating;
            return Action {
                consume: true,
                command: Some(Command::Start),
                ..Action::default()
            };
        }
        Action::default()
    }
}
