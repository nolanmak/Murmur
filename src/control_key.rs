//! The Control dictation key, as seen by the event tap. Control alone is
//! dictation. Control+Option (either order, held without another key) is
//! the same dictation marked "speak": the agent should answer it aloud.
use crate::space_hotkey::Key;

const FLAGS_CHANGED: u32 = 12;
const KEY_DOWN: u32 = 10;
const ESCAPE: i64 = 53;
const LEFT_OPTION: i64 = 58;
const RIGHT_OPTION: i64 = 61;
const CONTROL: u64 = 1 << 18;
const OPTION: u64 = 1 << 19;
/// Shift, Command and Fn: Control with these is a shortcut.
const SHORTCUT: u64 = (1 << 17) | (1 << 20) | (1 << 23);

#[derive(Default)]
pub struct ControlKey {
    down: bool,
    speak: bool,
}

impl ControlKey {
    pub fn held(&self) -> bool {
        self.down
    }

    /// Whether the current (or just-finished) Control hold had Option.
    pub fn speak(&self) -> bool {
        self.speak
    }

    /// `busy`: Murmur is processing, so a new hold passes through.
    pub fn event(&mut self, kind: u32, code: i64, flags: u64, busy: bool) -> Option<Key> {
        // Control is represented as flagsChanged; use the flag so both left
        // and right Control work on keyboards whose keycodes differ.
        match kind {
            FLAGS_CHANGED if flags & CONTROL != 0 && !self.down => {
                self.down = true;
                self.speak = flags & OPTION != 0;
                Some(if flags & SHORTCUT != 0 || busy {
                    Key::ModifiedDown
                } else {
                    Key::Down
                })
            }
            FLAGS_CHANGED if flags & CONTROL == 0 && self.down => {
                self.down = false;
                Some(Key::Up)
            }
            FLAGS_CHANGED if self.down && flags & SHORTCUT != 0 => Some(Key::Other),
            FLAGS_CHANGED
                if self.down
                    && flags & OPTION != 0
                    && matches!(code, LEFT_OPTION | RIGHT_OPTION) =>
            {
                self.speak = true;
                None
            }
            KEY_DOWN if code == ESCAPE => Some(Key::Escape),
            KEY_DOWN if self.down => Some(Key::Other),
            _ => None,
        }
    }
}
