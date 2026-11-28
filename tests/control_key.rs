//! Control is dictation; Control+Option is dictation the model should answer
//! aloud. These pin the existing Control behavior before Option was added.
use murmur::{control_key::ControlKey, space_hotkey::Key};

const FLAGS_CHANGED: u32 = 12;
const KEY_DOWN: u32 = 10;
const CONTROL: u64 = 1 << 18;
const SHIFT: u64 = 1 << 17;
const OPTION: u64 = 1 << 19;
const COMMAND: u64 = 1 << 20;
const LEFT_CONTROL: i64 = 59;
const LEFT_OPTION: i64 = 58;
const RIGHT_OPTION: i64 = 61;

fn key(k: Option<Key>) -> &'static str {
    match k {
        Some(Key::Down) => "down",
        Some(Key::Up) => "up",
        Some(Key::Other) => "other",
        Some(Key::ModifiedDown) => "modified",
        Some(Key::Escape) => "escape",
        Some(Key::Repeat) => "repeat",
        None => "none",
    }
}

#[test]
fn plain_control_is_unchanged_dictation() {
    let mut c = ControlKey::default();
    assert_eq!(
        key(c.event(FLAGS_CHANGED, LEFT_CONTROL, CONTROL, false)),
        "down"
    );
    assert!(!c.speak());
    assert_eq!(key(c.event(KEY_DOWN, 0, CONTROL, false)), "other");
    assert_eq!(key(c.event(KEY_DOWN, 53, CONTROL, false)), "escape");
    assert_eq!(key(c.event(FLAGS_CHANGED, LEFT_CONTROL, 0, false)), "up");
    assert!(!c.speak());
    // Keys while Control is up are not ours.
    assert_eq!(key(c.event(KEY_DOWN, 0, 0, false)), "none");
}

#[test]
fn control_with_shift_command_or_while_busy_passes_through() {
    let mut c = ControlKey::default();
    assert_eq!(
        key(c.event(FLAGS_CHANGED, LEFT_CONTROL, CONTROL | SHIFT, false)),
        "modified"
    );
    c.event(FLAGS_CHANGED, LEFT_CONTROL, 0, false);
    assert_eq!(
        key(c.event(FLAGS_CHANGED, LEFT_CONTROL, CONTROL | COMMAND, false)),
        "modified"
    );
    c.event(FLAGS_CHANGED, LEFT_CONTROL, 0, false);
    assert_eq!(
        key(c.event(FLAGS_CHANGED, LEFT_CONTROL, CONTROL, true)),
        "modified"
    );
    c.event(FLAGS_CHANGED, LEFT_CONTROL, 0, false);
    // Shift joining a held Control is a shortcut.
    c.event(FLAGS_CHANGED, LEFT_CONTROL, CONTROL, false);
    assert_eq!(
        key(c.event(FLAGS_CHANGED, 56, CONTROL | SHIFT, false)),
        "other"
    );
}

#[test]
fn control_with_option_held_first_is_a_speak_dictation() {
    let mut c = ControlKey::default();
    assert_eq!(
        key(c.event(FLAGS_CHANGED, LEFT_CONTROL, CONTROL | OPTION, false)),
        "down"
    );
    assert!(c.speak());
    assert_eq!(
        key(c.event(FLAGS_CHANGED, LEFT_CONTROL, OPTION, false)),
        "up"
    );
    assert!(
        c.speak(),
        "speak stays set for the dictation that just ended"
    );
    // The next plain Control hold is ordinary dictation again.
    c.event(FLAGS_CHANGED, LEFT_CONTROL, CONTROL, false);
    assert!(!c.speak());
}

#[test]
fn option_joining_a_held_control_makes_it_a_speak_dictation() {
    for option in [LEFT_OPTION, RIGHT_OPTION] {
        let mut c = ControlKey::default();
        c.event(FLAGS_CHANGED, LEFT_CONTROL, CONTROL, false);
        assert_eq!(
            key(c.event(FLAGS_CHANGED, option, CONTROL | OPTION, false)),
            "none"
        );
        assert!(c.speak());
        // Releasing Option first keeps it a speak dictation.
        assert_eq!(key(c.event(FLAGS_CHANGED, option, CONTROL, false)), "none");
        assert!(c.speak());
        assert_eq!(key(c.event(FLAGS_CHANGED, LEFT_CONTROL, 0, false)), "up");
    }
}

#[test]
fn control_option_with_a_key_is_still_a_shortcut() {
    // VoiceOver and app shortcuts (Control+Option+key) stay shortcuts.
    let mut c = ControlKey::default();
    c.event(FLAGS_CHANGED, LEFT_CONTROL, CONTROL | OPTION, false);
    assert_eq!(key(c.event(KEY_DOWN, 4, CONTROL | OPTION, false)), "other");
}
