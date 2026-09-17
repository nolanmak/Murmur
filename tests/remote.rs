use std::time::Duration;
use text_to_speech::remote::*;
fn t(ms: u64) -> Duration {
    Duration::from_millis(ms)
}
fn session() -> Session {
    Session {
        destination: Destination {
            session: 1,
            connection_generation: 1,
            window: 1,
        },
        foreground: true,
        connected: true,
        unlocked: true,
        isolated: true,
        clipboard_enabled: true,
        keyboard_enabled: true,
    }
}
fn controller() -> Controller {
    Controller::new(t(100), t(50))
}
fn begin(c: &mut Controller) -> Attempt {
    c.begin(true, "Café 👋 — hello", session(), Profile::Mac, t(0))
        .unwrap()
}
#[test]
fn sharing_requires_opt_in_and_valid_single_line_text() {
    let mut c = controller();
    assert_eq!(
        c.begin(false, "hello", session(), Profile::Mac, t(0)),
        Err(Failure::Disabled)
    );
    for text in [
        "",
        " ",
        "hi\n",
        "hi\r",
        "hi\t",
        "hi\0",
        "hi\u{2028}",
        "hi\u{2029}",
    ] {
        assert_eq!(
            c.begin(true, text, session(), Profile::Mac, t(0)),
            Err(Failure::InvalidText)
        );
    }
    assert!(
        c.begin(true, "Café 👋  spaced", session(), Profile::Mac, t(0))
            .is_ok()
    );
}
#[test]
fn explicit_attempt_shares_then_waits_for_receipt_then_dispatches_once() {
    let mut c = controller();
    let id = begin(&mut c);
    assert_eq!(c.state(), State::Preparing);
    assert_eq!(c.received(id, session(), t(1)), None);
    assert!(c.shared(id, session(), t(2)));
    assert_eq!(c.state(), State::WaitingForClipboard);
    assert!(!c.shared(id, session(), t(3)));
    assert_eq!(
        c.received(id, session(), t(4)),
        Some(Profile::Mac.shortcut())
    );
    assert_eq!(c.state(), State::PasteRequested);
    assert_eq!(c.received(id, session(), t(5)), None);
    assert!(c.dispatched(id, false, t(6)));
    assert_eq!(c.state(), State::PasteSent);
    assert!(!c.dispatched(id, true, t(7)));
    assert_eq!(c.state(), State::PasteSent);
}
#[test]
fn verified_insertion_is_a_separate_outcome() {
    let mut c = controller();
    let id = begin(&mut c);
    c.shared(id, session(), t(1));
    c.received(id, session(), t(2));
    assert!(c.dispatched(id, true, t(3)));
    assert_eq!(c.state(), State::Inserted);
}
#[test]
fn unavailable_destinations_fail_before_sharing() {
    for (s, expected) in [
        (
            Session {
                foreground: false,
                ..session()
            },
            Failure::DestinationChanged,
        ),
        (
            Session {
                connected: false,
                ..session()
            },
            Failure::Disconnected,
        ),
        (
            Session {
                unlocked: false,
                ..session()
            },
            Failure::Locked,
        ),
        (
            Session {
                isolated: false,
                ..session()
            },
            Failure::Ambiguous,
        ),
        (
            Session {
                clipboard_enabled: false,
                ..session()
            },
            Failure::ClipboardDisabled,
        ),
        (
            Session {
                keyboard_enabled: false,
                ..session()
            },
            Failure::KeyboardDenied,
        ),
    ] {
        assert_eq!(
            controller().begin(true, "hi", s, Profile::Mac, t(0)),
            Err(expected)
        );
    }
}
#[test]
fn destination_is_revalidated_before_share_and_dispatch() {
    for destination in [
        Destination {
            session: 2,
            ..session().destination
        },
        Destination {
            connection_generation: 2,
            ..session().destination
        },
        Destination {
            window: 2,
            ..session().destination
        },
    ] {
        let changed = Session {
            destination,
            ..session()
        };
        let mut c = controller();
        let id = begin(&mut c);
        assert!(!c.shared(id, changed, t(1)));
        assert_eq!(c.state(), State::Failed(Failure::DestinationChanged));
        let mut c = controller();
        let id = begin(&mut c);
        c.shared(id, session(), t(1));
        assert_eq!(c.received(id, changed, t(2)), None);
        assert_eq!(c.state(), State::Failed(Failure::DestinationChanged));
    }
}
#[test]
fn observed_disconnect_invalidates_even_if_connection_later_returns() {
    let mut c = controller();
    let id = begin(&mut c);
    c.shared(id, session(), t(1));
    c.observe(
        Session {
            connected: false,
            ..session()
        },
        t(2),
    );
    assert_eq!(c.received(id, session(), t(3)), None);
    assert_eq!(c.state(), State::Failed(Failure::Disconnected));
}
#[test]
fn cancellation_and_new_attempt_reject_all_stale_callbacks() {
    for stage in 0..3 {
        let mut c = controller();
        let old = begin(&mut c);
        if stage > 0 {
            c.shared(old, session(), t(1));
        }
        if stage > 1 {
            c.received(old, session(), t(2));
        }
        c.cancel();
        assert_eq!(c.state(), State::Cancelled);
        assert_eq!(c.received(old, session(), t(3)), None);
        assert!(!c.dispatched(old, true, t(3)));
        let new = begin(&mut c);
        assert_ne!(old, new);
        assert!(!c.shared(old, session(), t(4)));
        assert_eq!(c.state(), State::Preparing);
        assert!(c.shared(new, session(), t(4)));
    }
}
#[test]
fn double_click_cannot_replace_a_pending_attempt() {
    let mut c = controller();
    let id = begin(&mut c);
    assert_eq!(
        c.begin(true, "other", session(), Profile::Linux, t(1)),
        Err(Failure::Busy)
    );
    assert!(c.shared(id, session(), t(2)));
}
#[test]
fn synchronization_deadline_includes_preparation_and_rejects_boundary() {
    for at in [99, 100, 101] {
        let mut c = controller();
        let id = begin(&mut c);
        c.shared(id, session(), t(90));
        assert_eq!(c.received(id, session(), t(at)).is_some(), at < 100);
        if at >= 100 {
            assert_eq!(c.state(), State::Failed(Failure::TimedOut));
        }
    }
    let mut c = controller();
    let id = begin(&mut c);
    assert!(!c.shared(id, session(), t(100)));
}
#[test]
fn dispatch_deadline_prevents_late_success_and_requires_explicit_retry() {
    for at in [59, 60, 61] {
        let mut c = controller();
        let id = begin(&mut c);
        c.shared(id, session(), t(1));
        c.received(id, session(), t(10));
        assert_eq!(c.dispatched(id, false, t(at)), at < 60);
        if at >= 60 {
            assert_eq!(c.state(), State::Failed(Failure::TimedOut));
        }
        let next = c
            .begin(true, "retry", session(), Profile::Linux, t(70))
            .unwrap();
        assert_ne!(id, next);
        assert!(!c.dispatched(id, true, t(71)));
    }
}
#[test]
fn missing_callbacks_time_out_through_clock_observation() {
    let mut c = controller();
    begin(&mut c);
    c.observe(session(), t(100));
    assert_eq!(c.state(), State::Failed(Failure::TimedOut));
}
#[test]
fn profiles_only_generate_the_chosen_paste_chord_never_submit() {
    for (profile, command, control, shift) in [
        (Profile::Mac, true, false, false),
        (Profile::Linux, false, true, false),
        (Profile::LinuxTerminal, false, true, true),
    ] {
        let mut c = controller();
        let id = c
            .begin(true, "こんにちは", session(), profile, t(0))
            .unwrap();
        c.shared(id, session(), t(1));
        assert_eq!(
            c.received(id, session(), t(2)),
            Some(Shortcut {
                command,
                control,
                shift,
                key: 'v'
            })
        );
    }
}
