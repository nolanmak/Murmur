//! Seam-level tests: the rules from docs/REQUIREMENTS.md 6.5 that every
//! platform implementation must obey, exercised against fakes so they run on
//! CI with no audio device (5.6 "Testing strategy").

use fotw_audio::testing::{FakeTap, MockPlatform, SinkHandle, block_on};
use fotw_audio::{
    AudioPlatform, AudioTap, DeviceId, FormatRequest, FrameFlags, Permission, PermissionState,
    PlatformCaps, PlatformEvent, SampleFormat, StreamFormat, SystemScope, TapId, TapSession,
};

fn f48() -> StreamFormat {
    StreamFormat::new(48_000, 2, SampleFormat::F32)
}

fn f441() -> StreamFormat {
    StreamFormat::new(44_100, 2, SampleFormat::I16)
}

/// Seam rule 1: `FormatRequest` is a hint, `format()` after `start()` is truth.
#[test]
fn format_is_not_authoritative_before_start_and_is_after() {
    // The caller asks for 48 kHz stereo f32; the "device" will actually hand
    // back Windows process-loopback's fixed 44.1 kHz / 2 ch / S16.
    let mut tap = FakeTap::new(TapId::system_default(), f48()).with_start_formats([f441()]);

    assert!(
        !tap.format_is_authoritative(),
        "a tap must not claim authority over its format before start()"
    );
    assert_eq!(
        tap.format(),
        f48(),
        "before start() the tap may only echo the hint"
    );

    tap.start(SinkHandle::new().sink()).unwrap();

    assert!(tap.format_is_authoritative());
    assert_eq!(
        tap.format(),
        f441(),
        "after start() format() must report what the platform actually gave us"
    );
    assert!(
        !FormatRequest::from(f48()).matches(&tap.format()),
        "the hint was not honoured, and callers must be able to see that"
    );

    tap.stop().unwrap();
    assert!(
        !tap.format_is_authoritative(),
        "a stopped tap has no authoritative format until it is started again"
    );
}

/// CAP-07 acceptance: re-read the format on every rebuild, never assume
/// 48 kHz. A sample-rate change between two `start()` calls must be absorbed
/// by the live session, not force the caller to tear the session down.
#[test]
fn sample_rate_change_between_starts_does_not_restart_the_session() {
    let handle = SinkHandle::new();
    let tap = FakeTap::new(TapId::system_default(), f48()).with_start_formats([f48(), f441()]);
    let mut session = TapSession::new(tap);

    let first = session.start(handle.sink()).unwrap();
    assert_eq!(first.sample_rate_hz, 48_000);
    session.tap_mut().emit(&[0.5; 480]);

    // AirPods connect mid-meeting: the device comes back at a different rate.
    let second = session.restart(handle.sink()).unwrap();
    assert_eq!(second.sample_rate_hz, 44_100);
    session.tap_mut().emit(&[0.25; 882]);

    assert_eq!(
        session.observed_formats(),
        vec![f48(), f441()],
        "the session records every format it has been handed"
    );
    assert_eq!(
        session.format_changes(),
        1,
        "exactly one format change was seen"
    );
    assert_eq!(session.restarts(), 1);
    assert_eq!(
        session.format(),
        Some(f441()),
        "the session's authoritative format is the most recent one"
    );

    // The same session object carried both formats: frames from before and
    // after the rate change landed on one continuous stream of callbacks.
    assert_eq!(handle.calls(), 2);
    assert_eq!(handle.samples().len(), 480 + 882);
    assert!(handle.errors().is_empty());
}

/// Seam rule 4 / the Windows path: consent is a *capability*, not a universal
/// macOS-shaped "request and wait" flow. Onboarding renders off this.
#[test]
fn permission_state_not_applicable_round_trips_through_a_platform() {
    let platform = MockPlatform::windows_loopback();

    let caps = platform.caps();
    assert!(!caps.needs_consent_for_system);
    assert!(
        !caps.emits_silence_when_idle,
        "Windows endpoint loopback delivers no callbacks while nothing plays"
    );

    let state = platform.permission(Permission::SystemAudio);
    assert_eq!(state, PermissionState::NotApplicable);
    assert!(
        !state.requires_user_action(),
        "onboarding must not show a consent step for a platform that has none"
    );
    assert!(!state.is_blocking());

    // Requesting anyway is legal and must round-trip the same value rather
    // than inventing NotDetermined or hanging forever.
    let requested = block_on(platform.request_permission(Permission::SystemAudio));
    assert_eq!(requested, PermissionState::NotApplicable);

    // The mic leg on the same platform is a real, queryable permission.
    assert_eq!(
        platform.permission(Permission::Microphone),
        PermissionState::NotDetermined
    );
    assert!(
        platform
            .permission(Permission::Microphone)
            .requires_user_action()
    );
    assert_eq!(
        block_on(platform.request_permission(Permission::Microphone)),
        PermissionState::Granted
    );
}

/// Seam rule 5: `events()` exists from day one, or the macOS code will assume
/// a static device and break.
#[test]
fn platform_events_reach_every_subscriber() {
    let platform = MockPlatform::windows_loopback();
    let a = platform.events();
    let b = platform.events();

    platform.emit(PlatformEvent::DefaultOutputChanged);
    platform.emit(PlatformEvent::FormatChanged(TapId::system_default()));

    assert_eq!(a.recv().unwrap(), PlatformEvent::DefaultOutputChanged);
    assert_eq!(
        a.recv().unwrap(),
        PlatformEvent::FormatChanged(TapId::system_default())
    );
    assert_eq!(b.recv().unwrap(), PlatformEvent::DefaultOutputChanged);

    // A dropped subscriber must not wedge the publisher.
    drop(b);
    platform.emit(PlatformEvent::AppListChanged);
    assert_eq!(a.recv().unwrap(), PlatformEvent::AppListChanged);
}

/// Capability negotiation: a platform that cannot scope by app says so, and
/// asking anyway is a typed error rather than a silent full-system capture.
#[test]
fn unsupported_scope_is_a_typed_error_not_a_silent_downgrade() {
    let platform = MockPlatform::windows_loopback();
    assert!(!platform.caps().app_scoped);
    assert!(platform.capturable_apps().is_empty());

    let err = platform
        .open_system(
            SystemScope::Apps(vec![fotw_audio::AppRef::Pid(42)]),
            FormatRequest::any(),
        )
        .unwrap_err();
    assert!(err.is_unsupported(), "expected Unsupported, got {err:?}");

    // What it *can* do still works.
    let tap = platform
        .open_system(SystemScope::DefaultOutputMix, FormatRequest::any())
        .unwrap();
    assert_eq!(tap.id(), &TapId::system_default());
}

/// TapId is the cross-platform identity in 6.5: "mic:<uid>" | "system:default"
/// | "system:app:<key>".
#[test]
fn tap_ids_use_the_documented_wire_shape() {
    assert_eq!(
        TapId::mic("BuiltInMicrophoneDevice").as_str(),
        "mic:BuiltInMicrophoneDevice"
    );
    assert_eq!(TapId::system_default().as_str(), "system:default");
    assert_eq!(
        TapId::system_app("us.zoom.xos").as_str(),
        "system:app:us.zoom.xos"
    );
    assert_eq!(TapId::system_default().to_string(), "system:default");
}

/// Frame flags are a bitset with the WASAPI-compatible values from 6.5.
#[test]
fn frame_flags_behave_as_a_bitset() {
    assert_eq!(FrameFlags::SILENT.bits(), 1);
    assert_eq!(FrameFlags::DISCONTINUITY.bits(), 2);
    assert_eq!(FrameFlags::TIMESTAMP_ERROR.bits(), 4);

    let f = FrameFlags::SILENT | FrameFlags::DISCONTINUITY;
    assert!(f.contains(FrameFlags::SILENT));
    assert!(!f.contains(FrameFlags::TIMESTAMP_ERROR));
    assert!(f.intersects(FrameFlags::TIMESTAMP_ERROR | FrameFlags::SILENT));
    assert!(FrameFlags::empty().is_empty());
    assert_eq!(f - FrameFlags::SILENT, FrameFlags::DISCONTINUITY);
    assert_eq!(format!("{f:?}"), "FrameFlags(SILENT | DISCONTINUITY)");
}

/// The default capability set is "this platform can do nothing", so a UI that
/// gates affordances on caps hides them rather than failing at runtime.
#[test]
fn default_caps_advertise_nothing() {
    let caps = PlatformCaps::default();
    assert!(!caps.system_mix);
    assert!(!caps.app_scoped);
    assert!(!caps.exclude_scope);
    assert!(!caps.emits_silence_when_idle);
    assert!(!caps.needs_consent_for_system);
}

/// A platform with no backend refuses to open anything, with a typed error,
/// rather than pretending to succeed and recording silence.
#[test]
fn the_stub_platform_is_honest_about_being_a_stub() {
    let platform = fotw_audio::platform::StubPlatform::new();
    assert_eq!(platform.caps(), PlatformCaps::default());
    assert!(platform.mics().is_empty());
    assert!(platform.capturable_apps().is_empty());

    let mic_err = platform
        .open_mic(&DeviceId::new("default"), FormatRequest::any())
        .unwrap_err();
    assert!(mic_err.is_unsupported());

    let sys_err = platform
        .open_system(SystemScope::DefaultOutputMix, FormatRequest::any())
        .unwrap_err();
    assert!(sys_err.is_unsupported());

    // events() must exist and be well-behaved even on a stub.
    let _rx: std::sync::mpsc::Receiver<PlatformEvent> = platform.events();
}

/// `host()` returns whatever backend this build actually has, and the caller
/// never gains a `#[cfg]` — only the capabilities differ.
#[test]
fn the_host_platform_advertises_what_this_build_can_do() {
    let platform = fotw_audio::platform::host();
    let caps = platform.caps();

    // events() is seam rule 5 and must exist on every backend.
    let _rx: std::sync::mpsc::Receiver<PlatformEvent> = platform.events();

    #[cfg(target_os = "macos")]
    {
        // Core Audio process taps: whole-system mixdown, exclude-based
        // scoping, and a consent grant that genuinely exists.
        assert!(caps.system_mix);
        assert!(caps.exclude_scope);
        assert!(caps.needs_consent_for_system);

        // The grant cannot be queried — there is no API — and a denial is
        // delivered as silence. Reporting anything definite here would make
        // onboarding lie, so the only honest answer is Unobservable, and the
        // real check is a round-trip capture (`fotw doctor`).
        assert_eq!(
            platform.permission(fotw_audio::Permission::SystemAudio),
            fotw_audio::PermissionState::Unobservable
        );

        // Per-app scoping is not wired up. Advertising it would make the UI
        // offer a control that silently captures everything.
        assert!(!caps.app_scoped);
        let err = platform
            .open_system(
                SystemScope::Apps(vec![fotw_audio::AppRef::Pid(1)]),
                FormatRequest::any(),
            )
            .unwrap_err();
        assert!(err.is_unsupported());
    }

    #[cfg(not(target_os = "macos"))]
    {
        assert_eq!(caps, PlatformCaps::default());
        assert!(
            platform
                .open_system(SystemScope::DefaultOutputMix, FormatRequest::any())
                .unwrap_err()
                .is_unsupported()
        );
    }
}
