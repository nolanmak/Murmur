//! The activity probe: who is using audio right now.
//!
//! This is the *input* to meeting detection (MTG-03), and it is the half of
//! the detector that cannot be made platform-free — only the OS knows which
//! processes hold an audio client and whether the default input is running.
//! Everything here is therefore about the shape of the answer, not about the
//! Core Audio calls that produce it: the policy that turns these facts into
//! "a meeting is probably happening" lives above the seam, in `fotwd::detect`,
//! and is tested there with no device at all.

use fotw_audio::activity::{ActivityProbe, ActivitySnapshot, AudioClient, InputDevice, Transport};
use fotw_audio::testing::FixedActivityProbe;

#[test]
fn bluetooth_input_is_not_trustworthy_for_mic_hot_detection() {
    // Issue #22, and the reason mic-hot cannot be a *required* conjunct: a
    // Bluetooth mic can report inactive while it is being recorded, so the
    // detector silently never fires for AirPods users — plausibly most of the
    // target audience.
    assert!(!Transport::Bluetooth.mic_activity_is_trustworthy());
    assert!(!Transport::BluetoothLe.mic_activity_is_trustworthy());

    // The wired paths do report honestly, as far as anyone has documented.
    assert!(Transport::BuiltIn.mic_activity_is_trustworthy());
    assert!(Transport::Usb.mic_activity_is_trustworthy());
    assert!(Transport::Thunderbolt.mic_activity_is_trustworthy());

    // An unknown transport is treated as untrustworthy rather than as a
    // working detector. Being wrong in this direction costs a missed prompt;
    // the other direction costs a detector that never fires and never says so.
    assert!(!Transport::Unknown.mic_activity_is_trustworthy());
}

#[test]
fn continuity_capture_is_a_bluetooth_class_transport() {
    // "Use iPhone as microphone" is wireless in the wireless case and has the
    // same reporting risk. Wired continuity is treated as trustworthy.
    assert!(!Transport::ContinuityCaptureWireless.mic_activity_is_trustworthy());
    assert!(Transport::ContinuityCaptureWired.mic_activity_is_trustworthy());
}

#[test]
fn a_snapshot_reports_which_processes_hold_the_input() {
    let snapshot = ActivitySnapshot {
        clients: vec![
            AudioClient::new(101, Some("us.zoom.xos")).with_input(true),
            AudioClient::new(202, Some("com.spotify.client")).with_output(true),
        ],
        default_input: Some(InputDevice::new(
            "MacBook Pro Microphone",
            Transport::BuiltIn,
            true,
        )),
    };

    assert!(snapshot.client("us.zoom.xos").is_some());
    assert!(snapshot.client("com.apple.Safari").is_none());

    let holders: Vec<u32> = snapshot.input_holders().map(|c| c.pid).collect();
    assert_eq!(holders, vec![101], "only Zoom holds the input");
}

#[test]
fn a_snapshot_can_exclude_our_own_process() {
    // We hold the input ourselves while recording. A detector that counted
    // its own tap as evidence of a meeting would re-arm forever.
    let snapshot = ActivitySnapshot {
        clients: vec![AudioClient::new(7, Some("com.flyonthewall.fotw")).with_input(true)],
        default_input: None,
    };
    assert_eq!(snapshot.input_holders().count(), 1);
    assert_eq!(snapshot.input_holders_excluding(7).count(), 0);
}

#[test]
fn the_fake_probe_replays_what_it_was_given() {
    // The fake is what lets every crate above the seam test detection with no
    // conferencing app installed and no audio device present.
    let probe = FixedActivityProbe::new(ActivitySnapshot {
        clients: vec![AudioClient::new(1, Some("com.microsoft.teams2")).with_input(true)],
        default_input: Some(InputDevice::new("AirPods Pro", Transport::Bluetooth, false)),
    });

    let snapshot = probe.snapshot().expect("the fake never fails by default");
    assert_eq!(snapshot.clients.len(), 1);
    assert_eq!(
        snapshot.default_input.as_ref().map(|d| d.transport),
        Some(Transport::Bluetooth)
    );
    assert!(!snapshot.mic_is_trustworthy());
}

#[test]
fn an_unimplemented_platform_refuses_rather_than_reporting_a_quiet_machine() {
    // The stub is what Linux and Windows resolve to. If it answered with an
    // empty snapshot, detection would be permanently and silently off there
    // rather than absent — the same shape of failure as recording silence.
    let stub = fotw_audio::platform::StubPlatform::new();
    let err = stub
        .snapshot()
        .expect_err("an unimplemented probe must not report an empty machine");
    assert!(err.is_unsupported(), "{err}");
}

#[test]
fn a_probe_that_fails_says_so_rather_than_reporting_an_empty_machine() {
    // An empty snapshot means "nothing is using audio", which the detector
    // reads as "no meeting". A failed probe must not be able to masquerade as
    // that, or a broken probe silently disables detection forever.
    let probe = FixedActivityProbe::failing("process list unavailable");
    assert!(probe.snapshot().is_err());
}
