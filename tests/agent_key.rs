//! Agent key: holding Right Option talks to the agent; Control is untouched.
use murmur::{
    agent::key::RightOption,
    space_hotkey::{Command, HoldSpace, Key},
};

const FLAGS_CHANGED: u32 = 12;
const KEY_DOWN: u32 = 10;
const RIGHT_OPTION: i64 = 61;
const LEFT_OPTION: i64 = 58;
const ALT: u64 = 1 << 19;
const RIGHT_ALT_DEVICE: u64 = 0x40;
const LEFT_ALT_DEVICE: u64 = 0x20;
const CONTROL: u64 = 1 << 18;

fn is(key: Option<Key>, want: &str) -> bool {
    matches!(
        (key, want),
        (Some(Key::Down), "down")
            | (Some(Key::Up), "up")
            | (Some(Key::Other), "other")
            | (Some(Key::ModifiedDown), "modified")
            | (Some(Key::Escape), "escape")
            | (None, "none")
    )
}

#[test]
fn right_option_press_and_release_are_hold_edges() {
    let mut k = RightOption::default();
    assert!(is(
        k.event(FLAGS_CHANGED, RIGHT_OPTION, ALT | RIGHT_ALT_DEVICE),
        "down"
    ));
    assert!(k.held());
    assert!(is(k.event(FLAGS_CHANGED, RIGHT_OPTION, 0), "up"));
    assert!(!k.held());
}

#[test]
fn left_option_and_control_are_not_the_agent_key() {
    let mut k = RightOption::default();
    assert!(is(
        k.event(FLAGS_CHANGED, LEFT_OPTION, ALT | LEFT_ALT_DEVICE),
        "none"
    ));
    assert!(is(k.event(FLAGS_CHANGED, 59, CONTROL), "none"));
    assert!(is(k.event(KEY_DOWN, 0, 0), "none"));
    assert!(!k.held());
}

#[test]
fn typing_with_right_option_or_another_modifier_is_a_shortcut() {
    let mut k = RightOption::default();
    // Right Option already combined with Control: a shortcut, never a hold.
    assert!(is(
        k.event(
            FLAGS_CHANGED,
            RIGHT_OPTION,
            ALT | RIGHT_ALT_DEVICE | CONTROL
        ),
        "modified"
    ));
    k.event(FLAGS_CHANGED, RIGHT_OPTION, 0);
    k.event(FLAGS_CHANGED, RIGHT_OPTION, ALT | RIGHT_ALT_DEVICE);
    // Option+e (accent) while held.
    assert!(is(k.event(KEY_DOWN, 14, ALT | RIGHT_ALT_DEVICE), "other"));
    assert!(is(k.event(KEY_DOWN, 53, ALT | RIGHT_ALT_DEVICE), "escape"));
}

#[test]
fn only_a_long_quiet_hold_records_for_the_agent() {
    // Long hold: starts after 350 ms, finishes on release.
    let mut k = RightOption::default();
    let mut hold = HoldSpace::default();
    hold.key(
        k.event(FLAGS_CHANGED, RIGHT_OPTION, ALT | RIGHT_ALT_DEVICE)
            .unwrap(),
        0,
    );
    assert_eq!(hold.tick(200).command, None);
    assert_eq!(hold.tick(400).command, Some(Command::Start));
    let up = hold.key(k.event(FLAGS_CHANGED, RIGHT_OPTION, 0).unwrap(), 2000);
    assert_eq!(up.command, Some(Command::Finish));
    // Quick tap: nothing.
    hold.key(
        k.event(FLAGS_CHANGED, RIGHT_OPTION, ALT | RIGHT_ALT_DEVICE)
            .unwrap(),
        3000,
    );
    assert_eq!(
        hold.key(k.event(FLAGS_CHANGED, RIGHT_OPTION, 0).unwrap(), 3100)
            .command,
        None
    );
    assert_eq!(hold.tick(4000).command, None);
    // Option+letter before the hold threshold: never starts.
    hold.key(
        k.event(FLAGS_CHANGED, RIGHT_OPTION, ALT | RIGHT_ALT_DEVICE)
            .unwrap(),
        5000,
    );
    hold.key(k.event(KEY_DOWN, 14, ALT | RIGHT_ALT_DEVICE).unwrap(), 5100);
    assert_eq!(hold.tick(5500).command, None);
}
