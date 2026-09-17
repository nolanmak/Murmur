//! `FileAudioSource` is the reason the rest of the pipeline is testable at all.
//!
//! Device-dependent CI is close to unachievable for this project: GitHub macOS
//! runners have recurring null-audio-device regressions, and Core Audio taps
//! additionally need a signed binary plus a TCC grant that cannot be given
//! non-interactively. So ~90% of coverage rides on replaying fixtures through
//! the real seam (docs/REQUIREMENTS.md 5.6). These tests are the guarantee
//! that the fake is faithful enough to carry that weight.

use std::time::{Duration, Instant};

use fotw_audio::platform::file::{FileAudioSource, FilePlatform, ReplaySpeed};
use fotw_audio::testing::SinkHandle;
use fotw_audio::wav::{self, WavData};
use fotw_audio::{
    AudioPlatform, AudioTap, FormatRequest, FrameFlags, SampleFormat, StreamFormat, SystemScope,
    TapId,
};

/// `seconds` of a 440 Hz tone at 16 kHz mono.
fn tone(seconds: f32) -> WavData {
    let format = StreamFormat::new(16_000, 1, SampleFormat::I16);
    let n = (16_000.0 * seconds) as usize;
    let samples = (0..n)
        .map(|i| {
            let t = i as f32 / 16_000.0;
            (t * 440.0 * std::f32::consts::TAU).sin() * 0.5
        })
        .collect();
    WavData { format, samples }
}

fn silence(seconds: f32) -> WavData {
    let format = StreamFormat::new(16_000, 1, SampleFormat::I16);
    WavData {
        format,
        samples: vec![0.0; (16_000.0 * seconds) as usize],
    }
}

#[test]
fn replays_every_frame_of_the_fixture() {
    let data = tone(0.5);
    let expected_frames = data.frames();
    let handle = SinkHandle::new();
    let mut src = FileAudioSource::from_wav(TapId::system_default(), data, ReplaySpeed::Unpaced);

    assert_eq!(src.total_frames(), expected_frames);
    src.start(handle.sink()).unwrap();
    src.wait_for_completion();

    assert_eq!(
        handle.samples().len(),
        expected_frames,
        "mono fixture: one sample per frame, none dropped and none invented"
    );
    assert_eq!(src.delivered_frames(), expected_frames as u64);
    assert!(handle.errors().is_empty());
}

#[test]
fn format_is_authoritative_only_after_start() {
    let handle = SinkHandle::new();
    let mut src =
        FileAudioSource::from_wav(TapId::system_default(), tone(0.02), ReplaySpeed::Unpaced);

    assert!(!src.format_is_authoritative());
    src.start(handle.sink()).unwrap();
    assert!(src.format_is_authoritative());
    assert_eq!(src.format().sample_rate_hz, 16_000);

    src.stop().unwrap();
    assert!(
        !src.format_is_authoritative(),
        "a stopped tap has no authoritative format until started again"
    );
}

#[test]
fn timestamps_are_monotonic_on_both_clocks() {
    let handle = SinkHandle::new();
    let mut src =
        FileAudioSource::from_wav(TapId::system_default(), tone(0.2), ReplaySpeed::Unpaced);
    src.start(handle.sink()).unwrap();
    src.wait_for_completion();

    assert!(handle.calls() > 1, "the fixture spans several buffers");
    assert!(
        handle.timestamps_are_monotonic(),
        "device_frames and host_ns must both advance; a regression here means \
         the backend is stamping from more than one clock, which silently \
         corrupts two-stream alignment"
    );

    // device_frames is a running count, so the last one plus its buffer must
    // equal the total delivered.
    let last = *handle.timestamps().last().unwrap();
    assert!(last.device_frames < src.delivered_frames());
}

/// Seam rule 3, the half a fixture tap can get wrong on its own: `host_ns` is
/// the *process-wide* clock, so two taps started apart stamp apart.
///
/// This tap used to time its stamps from its own worker's start, which reads
/// identically inside one tap and is exactly backwards across two: both legs
/// stamped their first buffer at ≈0 however far apart they really began, so
/// `fotwd`'s `leg_anchors` (#86) measured every fixture-driven session as two
/// taps that woke together and silently declined to anchor anything. With ~90%
/// of this project's coverage riding on this tap, that put #86 out of reach of
/// almost every test in the suite.
#[test]
fn two_taps_started_apart_stamp_their_first_buffers_apart() {
    /// Long enough that the gap cannot be lost to scheduler granularity, short
    /// enough to stay a unit test.
    const APART: Duration = Duration::from_millis(50);

    let first = SinkHandle::new();
    let mut earlier =
        FileAudioSource::from_wav(TapId::system_default(), tone(0.05), ReplaySpeed::Unpaced);
    let before_earlier = fotw_audio::clock::host_ns();
    earlier.start(first.sink()).unwrap();
    earlier.wait_for_completion();

    std::thread::sleep(APART);

    let second = SinkHandle::new();
    let mut later =
        FileAudioSource::from_wav(TapId::mic("fixture-mic"), tone(0.05), ReplaySpeed::Unpaced);
    later.start(second.sink()).unwrap();
    later.wait_for_completion();

    let earlier_t0 = first.timestamps()[0].host_ns;
    let later_t0 = second.timestamps()[0].host_ns;

    assert!(
        earlier_t0 >= before_earlier,
        "the first buffer is stamped on the shared clock, so it cannot predate \
         a reading taken before start(): {earlier_t0} < {before_earlier}"
    );
    assert!(
        later_t0.saturating_sub(earlier_t0) >= APART.as_nanos() as u64,
        "two taps started {APART:?} apart stamped their first buffers only \
         {}ns apart — a tap timing from its own start makes every pair of legs \
         look simultaneous",
        later_t0.saturating_sub(earlier_t0)
    );
}

/// Buffers are 10 ms, because that is the unit the AEC requires — it panics,
/// rather than erroring, on a frame that is not exactly 10 ms at its rate.
/// Driving the whole chain in 10 ms units makes the framing line up for free.
#[test]
fn delivers_ten_millisecond_buffers() {
    let handle = SinkHandle::new();
    let mut src =
        FileAudioSource::from_wav(TapId::system_default(), tone(1.0), ReplaySpeed::Unpaced);
    src.start(handle.sink()).unwrap();
    src.wait_for_completion();

    // 16 kHz mono => 160 samples per 10 ms buffer, 100 buffers in a second.
    assert_eq!(handle.calls(), 100);
    let ts = handle.timestamps();
    assert_eq!(ts[1].device_frames - ts[0].device_frames, 160);
}

/// A 90-minute meeting has to run inside a CI step, so the multiplier has to
/// actually work rather than merely exist.
///
/// The bound is a ratio, not a wall-clock constant, and it is set to separate
/// correct behaviour from a specific regression with margin on both sides.
/// Pacing with a `thread::sleep` per 10 ms chunk *looks* right and silently
/// collapses to about 4x, because a 200 µs sleep rounds up to the scheduler's
/// millisecond granularity. At 50x a correct implementation finishes in
/// `seconds/50`; the regression takes roughly `seconds/4`. Asserting
/// `seconds/10` leaves ~5x headroom either way, so it neither flakes on a
/// loaded runner nor passes the bug.
#[test]
fn speed_multiplier_replays_materially_faster_than_real_time() {
    let seconds = 4.0;
    let handle = SinkHandle::new();
    let mut src = FileAudioSource::from_wav(
        TapId::system_default(),
        tone(seconds),
        ReplaySpeed::Multiplier(50.0),
    );

    let began = Instant::now();
    src.start(handle.sink()).unwrap();
    src.wait_for_completion();
    let elapsed = began.elapsed();

    assert_eq!(handle.samples().len(), (16_000.0 * seconds) as usize);
    assert!(
        elapsed.as_secs_f32() < seconds / 10.0,
        "{seconds}s of fixture at 50x must complete at >=10x real time \
         (expected well under {:.0}ms), took {elapsed:?}",
        seconds * 100.0
    );
}

/// A genuinely silent fixture must be reported as silent. This is what lets
/// the layer above tell "quiet room" from "the tap died" — the macOS 26 defect
/// where taps deliver all-zero buffers for minutes while the IOProc keeps
/// firing normally (CAP-05).
#[test]
fn silent_buffers_are_flagged_and_audible_ones_are_not() {
    let handle = SinkHandle::new();
    let mut src =
        FileAudioSource::from_wav(TapId::system_default(), silence(0.05), ReplaySpeed::Unpaced);
    src.start(handle.sink()).unwrap();
    src.wait_for_completion();
    assert!(
        handle
            .flags()
            .iter()
            .all(|f| f.contains(FrameFlags::SILENT)),
        "every buffer of a zero fixture must carry SILENT"
    );

    let handle = SinkHandle::new();
    let mut src =
        FileAudioSource::from_wav(TapId::system_default(), tone(0.05), ReplaySpeed::Unpaced);
    src.start(handle.sink()).unwrap();
    src.wait_for_completion();
    assert!(
        handle
            .flags()
            .iter()
            .all(|f| !f.contains(FrameFlags::SILENT)),
        "a 440 Hz tone is not silence"
    );
}

#[test]
fn stopping_midway_stops_delivery_and_is_idempotent() {
    let handle = SinkHandle::new();
    let mut src =
        FileAudioSource::from_wav(TapId::system_default(), tone(30.0), ReplaySpeed::Realtime);
    src.start(handle.sink()).unwrap();
    src.stop().unwrap();
    let after_stop = handle.samples().len();

    src.stop().unwrap();
    assert_eq!(
        handle.samples().len(),
        after_stop,
        "a second stop() delivers nothing further and does not error"
    );
    assert!(
        after_stop < 30 * 16_000,
        "stopping must actually interrupt replay"
    );
}

#[test]
fn starting_twice_is_a_typed_error_not_a_second_worker() {
    let handle = SinkHandle::new();
    let mut src =
        FileAudioSource::from_wav(TapId::system_default(), tone(0.05), ReplaySpeed::Unpaced);
    src.start(handle.sink()).unwrap();
    let err = src.start(handle.sink()).unwrap_err();
    assert!(!err.is_unsupported(), "double start is a platform error");
    src.stop().unwrap();
}

#[test]
fn file_platform_serves_taps_through_the_ordinary_seam() {
    let platform = FilePlatform::new(ReplaySpeed::Unpaced)
        .with_mic(tone(0.05))
        .with_system(tone(0.05));

    assert!(platform.caps().system_mix);
    assert_eq!(platform.mics().len(), 1);

    let handle = SinkHandle::new();
    let mut tap = platform
        .open_system(SystemScope::DefaultOutputMix, FormatRequest::any())
        .unwrap();
    assert_eq!(tap.id(), &TapId::system_default());
    tap.start(handle.sink()).unwrap();
    tap.stop().unwrap();

    // Seam rule 2: the two legs are separate taps and are never pre-mixed.
    let mic = platform
        .open_mic(
            &fotw_audio::DeviceId::new("fixture-mic"),
            FormatRequest::any(),
        )
        .unwrap();
    assert!(mic.id().is_mic());
    assert!(tap.id().is_system());
}

#[test]
fn a_scope_the_file_platform_cannot_serve_is_a_typed_error() {
    let platform = FilePlatform::new(ReplaySpeed::Unpaced).with_system(tone(0.05));
    let err = platform
        .open_system(
            SystemScope::Apps(vec![fotw_audio::AppRef::Pid(1)]),
            FormatRequest::any(),
        )
        .unwrap_err();
    assert!(err.is_unsupported());
}

#[test]
fn fixtures_round_trip_through_a_real_wav_file() {
    let data = tone(0.05);
    let bytes = wav::encode_i16(data.format, &data.samples);
    let dir = std::env::temp_dir().join("fotw-audio-tests");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("tone.wav");
    std::fs::write(&path, bytes).unwrap();

    let handle = SinkHandle::new();
    let mut src =
        FileAudioSource::open(TapId::system_default(), &path, ReplaySpeed::Unpaced).unwrap();
    src.start(handle.sink()).unwrap();
    src.wait_for_completion();

    assert_eq!(handle.samples().len(), data.samples.len());
    std::fs::remove_file(&path).ok();
}
