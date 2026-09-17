//! How long to wait for the OS credential store before giving up.
//!
//! # Why five seconds was the wrong number everywhere
//!
//! The deadline exists for liveness: under launchd, in CI, or over SSH, macOS
//! raises an approval dialog that nobody can see, and the call blocks forever
//! with no output and no error. A daemon that hangs silently on startup is
//! worse than one that refuses to start.
//!
//! But the same five seconds applied to a person sitting in front of a real
//! dialog, and no one finds a window, reads it, and clicks "Always Allow" in
//! five seconds. So the interactive case timed out too — and because the
//! request is abandoned rather than cancelled, the user's eventual click
//! landed on a request nobody was waiting for any more. They clicked Allow and
//! the command had already failed. That happened repeatedly.
//!
//! The deadline should therefore ask a different question: is there anybody
//! who could answer? Not "how long is reasonable".

use std::time::Duration;

use fotw_secrets::{Answerable, keychain_timeout};

#[test]
fn a_session_nobody_is_watching_gives_up_quickly() {
    let t = keychain_timeout(Answerable::Nobody);
    assert!(
        t <= Duration::from_secs(5),
        "a headless run must fail fast, not hang: {t:?}"
    );
    assert!(t > Duration::ZERO);
}

/// Long enough to find the window, read it, and click the right button.
#[test]
fn a_session_with_a_person_in_it_waits_long_enough_to_answer() {
    let t = keychain_timeout(Answerable::Person);
    assert!(
        t >= Duration::from_secs(30),
        "nobody clicks a dialog they have not found yet: {t:?}"
    );
}

/// Still bounded. "Wait forever" is the hang this deadline exists to prevent,
/// and an interactive session can still be one where the dialog fails to draw.
#[test]
fn even_an_interactive_wait_is_bounded() {
    assert!(keychain_timeout(Answerable::Person) <= Duration::from_secs(120));
}

#[test]
fn the_interactive_wait_is_longer_than_the_headless_one() {
    assert!(keychain_timeout(Answerable::Person) > keychain_timeout(Answerable::Nobody));
}

// ------------------------------------------------------------- detection

use fotw_secrets::answerable_from;

/// CI is the case the short deadline was written for.
#[test]
fn ci_is_nobody() {
    assert_eq!(
        answerable_from(&[("CI", "true")], true),
        Answerable::Nobody,
        "a CI runner has no one to click anything"
    );
}

/// An SSH session has a person, but not one who can see a macOS window server
/// dialog on the far end.
#[test]
fn ssh_is_nobody() {
    assert_eq!(
        answerable_from(&[("SSH_CONNECTION", "10.0.0.1 22 10.0.0.2 22")], true),
        Answerable::Nobody
    );
    assert_eq!(
        answerable_from(&[("SSH_TTY", "/dev/ttys004")], true),
        Answerable::Nobody
    );
}

/// No GUI session at all — a launchd daemon in the system context.
#[test]
fn no_window_server_is_nobody() {
    assert_eq!(answerable_from(&[], false), Answerable::Nobody);
}

/// The ordinary case: a person at the machine, running the app or the CLI.
#[test]
fn a_plain_local_run_has_a_person() {
    assert_eq!(answerable_from(&[], true), Answerable::Person);
}

/// CI wins even where a window server exists, because a hosted macOS runner
/// has both and still nobody to click.
#[test]
fn ci_beats_the_presence_of_a_gui() {
    assert_eq!(answerable_from(&[("CI", "1")], true), Answerable::Nobody);
}

/// An empty value is not a signal. Some shells export `CI=` unset-but-present.
#[test]
fn an_empty_marker_is_not_a_signal() {
    assert_eq!(answerable_from(&[("CI", "")], true), Answerable::Person);
}
