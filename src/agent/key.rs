//! The agent key: holding either Option key on its own talks to the active
//! agent session. Control stays Murmur's dictation key; this never reads it.
//! Option shortcuts (Option+key, Option-click, Option-drag) cancel the hold.
use crate::space_hotkey::Key;

const FLAGS_CHANGED: u32 = 12;
const KEY_DOWN: u32 = 10;
const LEFT_OPTION: i64 = 58;
const RIGHT_OPTION: i64 = 61;
const LEFT_MOUSE_DOWN: u32 = 1;
const RIGHT_MOUSE_DOWN: u32 = 3;
const ESCAPE: i64 = 53;
/// Device-dependent left- and right-Option bits in CGEventFlags.
const ALT_DEVICE: u64 = 0x20 | 0x40;
/// Shift, Control, Command and Fn.
const OTHER_MODIFIERS: u64 = (1 << 17) | (1 << 18) | (1 << 20) | (1 << 23);

/// Turns raw event-tap events into hold edges for [`crate::space_hotkey::HoldSpace`].
#[derive(Default)]
pub struct OptionKey {
    down: bool,
}

impl OptionKey {
    pub fn held(&self) -> bool {
        self.down
    }

    /// `kind`: CGEventType (10 keyDown, 12 flagsChanged); `code`: keycode.
    pub fn event(&mut self, kind: u32, code: i64, flags: u64) -> Option<Key> {
        match (kind, code) {
            (FLAGS_CHANGED, LEFT_OPTION | RIGHT_OPTION) => {
                // Held while either Option is down; released when both are up.
                let pressed = flags & ALT_DEVICE != 0;
                if pressed && !self.down {
                    self.down = true;
                    Some(if flags & OTHER_MODIFIERS != 0 {
                        Key::ModifiedDown
                    } else {
                        Key::Down
                    })
                } else if !pressed && self.down {
                    self.down = false;
                    Some(Key::Up)
                } else {
                    None
                }
            }
            // Another modifier joins the held Option: a shortcut.
            (FLAGS_CHANGED, _) if self.down && flags & OTHER_MODIFIERS != 0 => Some(Key::Other),
            (KEY_DOWN, ESCAPE) if self.down => Some(Key::Escape),
            // Option-click / Option-drag.
            (LEFT_MOUSE_DOWN | RIGHT_MOUSE_DOWN, _) if self.down => Some(Key::Other),
            // Option+letter (accents, symbols) while held.
            (KEY_DOWN, _) if self.down => Some(Key::Other),
            _ => None,
        }
    }
}
