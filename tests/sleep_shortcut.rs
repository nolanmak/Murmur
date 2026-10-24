use murmur::space_hotkey::{HoldSpace, Key};
#[test]
fn sleep_discards_pending_hold_without_replaying_space_on_wake() {
    let mut hold = HoldSpace::default();
    hold.key(Key::Down, 0);
    hold.suspend();
    assert!(hold.tick(60_000).command.is_none());
    assert!(!hold.key(Key::Up, 60_001).replay);
    hold.key(Key::Down, 60_002);
    assert!(hold.tick(90_000).command.is_some());
}

#[test]
fn sleep_while_space_is_idle_allows_a_fresh_deliberate_hold() {
    let mut hold = HoldSpace::default();
    hold.suspend();
    hold.key(Key::Down, 1);
    assert!(hold.tick(30_000).command.is_some());
}
