use text_to_speech::space_hotkey::{Command, HOLD_MS, HoldSpace, Key};
#[test]
fn quick_tap_replays_control_without_recording() {
    let mut h = HoldSpace::default();
    assert!(h.key(Key::Down, 0).consume);
    let a = h.key(Key::Up, 80);
    assert!(a.replay);
    assert!(!a.consume);
    assert_eq!(a.command, None);
}
#[test]
fn hold_starts_once_and_release_finishes_without_typing() {
    let mut h = HoldSpace::default();
    h.key(Key::Down, 0);
    assert_eq!(h.tick(HOLD_MS - 1).command, None);
    assert_eq!(h.tick(HOLD_MS).command, Some(Command::Start));
    assert_eq!(h.tick(HOLD_MS + 1).command, None);
    let up = h.key(Key::Up, HOLD_MS + 100);
    assert!(up.consume);
    assert!(!up.replay);
    assert_eq!(up.command, Some(Command::Finish));
}
#[test]
fn other_key_during_pending_control_is_replayed() {
    let mut h = HoldSpace::default();
    h.key(Key::Down, 0);
    let a = h.key(Key::Other, 20);
    assert!(a.replay);
    assert!(!a.consume);
    assert_eq!(h.tick(1000).command, None);
    assert!(!h.key(Key::Up, 1001).consume);
}
#[test]
fn modified_control_and_repeat_events_preserve_shortcuts() {
    let mut h = HoldSpace::default();
    assert!(!h.key(Key::ModifiedDown, 0).consume);
    assert_eq!(h.tick(1000).command, None);
    h.key(Key::Up, 1001);
    h.key(Key::Down, 2000);
    h.key(Key::Repeat, 2100);
    assert_eq!(h.tick(2000 + HOLD_MS).command, Some(Command::Start));
}
#[test]
fn escape_cancels_and_release_cannot_insert() {
    let mut h = HoldSpace::default();
    h.key(Key::Down, 0);
    h.tick(HOLD_MS);
    let a = h.key(Key::Escape, 400);
    assert!(a.consume);
    assert_eq!(a.command, Some(Command::Cancel));
    assert_eq!(h.key(Key::Up, 500).command, Some(Command::Finish));
}
