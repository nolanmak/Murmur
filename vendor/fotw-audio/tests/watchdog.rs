//! CAP-05 / CAP-06: the tap that dies without saying so.
//!
//! Every test here runs on a **manual clock** and never sleeps. A watchdog
//! whose thresholds are 5 s and 8 s cannot be tested in real time without
//! adding 13 s to CI per case, so the clock is injected and the whole crate's
//! detection logic is a pure function of `(now_ns, counters, corroboration)`.
//!
//! No audio device is touched: taps are [`FakeTap`]s and the activity counters
//! are supplied by the test directly, exactly as the real ones are supplied by
//! atomics bumped on the IOProc.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use fotw_audio::device_change::{DeviceChangeKind, DeviceChangeSignal};
use fotw_audio::supervisor::{
    CaptureSupervisor, GapKind, GapReason, HealthEvent, PollOutcome, SupervisorConfig,
};
use fotw_audio::testing::{FakeTap, ManualClock, SinkHandle, TapEvent, TapLog};
use fotw_audio::watchdog::{
    Fault, OutputActivity, OutputProbe, TapActivity, Verdict, Watchdog, WatchdogConfig,
};
use fotw_audio::{AudioTap, SampleFormat, StreamFormat, TapError, TapId};

fn f48() -> StreamFormat {
    StreamFormat::new(48_000, 2, SampleFormat::F32)
}

fn f16() -> StreamFormat {
    // What a Bluetooth headset engaging HFP drops the whole chain to.
    StreamFormat::new(16_000, 1, SampleFormat::F32)
}

/// One IOProc period. The real one is ~10 ms; the watchdog is polled far more
/// slowly than that, so the tests use a 100 ms tick to keep the loops short.
const TICK: Duration = Duration::from_millis(100);

/// A macOS-shaped config: the tap delivers callbacks even in a silent room, so
/// zero buffers is unambiguous.
fn macos_config() -> WatchdogConfig {
    WatchdogConfig::default()
}

/// A Windows-endpoint-loopback-shaped config: no callbacks at all while the
/// endpoint is idle, so zero buffers proves nothing on its own.
fn silent_when_idle_config() -> WatchdogConfig {
    WatchdogConfig {
        emits_silence_when_idle: false,
        ..WatchdogConfig::default()
    }
}

/// An [`OutputProbe`] that counts how often it was asked.
///
/// On macOS answering costs a `kAudioHardwarePropertyProcessObjectList` walk
/// plus one property read per process, so "did we ask when we did not need
/// to" is a real question and not a stylistic one.
#[derive(Debug)]
struct CountingProbe {
    answer: OutputActivity,
    calls: AtomicUsize,
}

impl CountingProbe {
    fn new(answer: OutputActivity) -> Self {
        Self {
            answer,
            calls: AtomicUsize::new(0),
        }
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::Relaxed)
    }
}

impl OutputProbe for CountingProbe {
    fn output_activity(&self) -> OutputActivity {
        self.calls.fetch_add(1, Ordering::Relaxed);
        self.answer
    }
}

// ---------------------------------------------------------------- watchdog

/// The baseline that everything else is measured against: a tap delivering
/// real audio must never be declared faulty, however long the meeting runs.
#[test]
fn a_healthy_tap_never_faults_over_a_long_meeting() {
    let mut wd = Watchdog::new(macos_config());
    let mut now = 0u64;
    wd.arm(now);

    let mut activity = TapActivity::default();
    // 45 minutes at one poll per 100 ms.
    for _ in 0..27_000 {
        now += TICK.as_nanos() as u64;
        activity.buffers += 1;
        activity.frames += 4_800;
        assert_eq!(
            wd.poll(now, activity, &OutputActivity::Active),
            Verdict::Healthy
        );
    }
}

/// The zero-buffer case. On a platform that emits silence when idle — macOS —
/// the absence of callbacks needs no corroboration at all: the IOProc firing
/// is unconditional, so its stopping is proof on its own.
#[test]
fn no_buffers_for_the_timeout_is_a_fault_with_no_corroboration_needed() {
    let cfg = macos_config();
    let timeout = cfg.no_buffer_timeout;
    let mut wd = Watchdog::new(cfg);
    wd.arm(0);

    // One good buffer at t=0 so there is a "last seen" to measure from.
    let activity = TapActivity {
        buffers: 1,
        silent_buffers: 0,
        frames: 4_800,
    };
    assert_eq!(
        wd.poll(0, activity, &OutputActivity::Unknown),
        Verdict::Healthy
    );

    let just_under = timeout.as_nanos() as u64 - 1;
    assert_eq!(
        wd.poll(just_under, activity, &OutputActivity::Unknown),
        Verdict::Healthy,
        "the fault must not fire one nanosecond early"
    );

    let at = timeout.as_nanos() as u64;
    let verdict = wd.poll(at, activity, &OutputActivity::Unknown);
    let Verdict::Faulted(Fault::NoBuffers { since_ns, for_ns }) = verdict else {
        panic!("expected a NoBuffers fault, got {verdict:?}");
    };
    assert_eq!(
        since_ns, 0,
        "the gap starts at the last buffer that arrived"
    );
    assert_eq!(for_ns, at);
}

/// A tap that never delivers a single buffer is the "IOProc was never
/// registered" failure — `AudioDeviceCreateIOProcIDWithBlock(None, ..)` on
/// macOS 26 does exactly this and reports success.
#[test]
fn a_tap_that_never_delivers_anything_faults_from_the_moment_it_was_armed() {
    let cfg = macos_config();
    let timeout = cfg.no_buffer_timeout.as_nanos() as u64;
    let mut wd = Watchdog::new(cfg);
    wd.arm(1_000);

    assert_eq!(
        wd.poll(
            1_000 + timeout - 1,
            TapActivity::default(),
            &OutputActivity::Unknown
        ),
        Verdict::Healthy
    );
    let verdict = wd.poll(
        1_000 + timeout,
        TapActivity::default(),
        &OutputActivity::Unknown,
    );
    let Verdict::Faulted(Fault::NoBuffers { since_ns, .. }) = verdict else {
        panic!("expected NoBuffers, got {verdict:?}");
    };
    assert_eq!(since_ns, 1_000, "measured from the arm, not from zero");
}

/// Seam rule 4 has teeth here. Where `emits_silence_when_idle` is false —
/// Windows endpoint loopback — *no callbacks at all* is the normal state of a
/// machine playing nothing, so the same evidence that is conclusive on macOS
/// proves nothing, and firing on it would rebuild the tap every time the user
/// paused their music.
#[test]
fn where_the_platform_goes_quiet_when_idle_zero_buffers_needs_corroboration() {
    let cfg = silent_when_idle_config();
    let timeout = cfg.no_buffer_timeout.as_nanos() as u64;

    let mut idle = Watchdog::new(cfg);
    idle.arm(0);
    assert!(
        matches!(
            idle.poll(timeout * 10, TapActivity::default(), &OutputActivity::Idle),
            Verdict::Healthy | Verdict::NoAudio { .. }
        ),
        "nothing is playing, so nothing is wrong"
    );

    let mut busy = Watchdog::new(cfg);
    busy.arm(0);
    assert!(
        matches!(
            busy.poll(timeout, TapActivity::default(), &OutputActivity::Active),
            Verdict::Faulted(Fault::NoBuffers { .. })
        ),
        "something IS playing and we are getting nothing: that is the fault"
    );
}

/// The single most important negative result in this file. A meeting where
/// nobody happens to be talking produces bit-exact zero buffers indefinitely,
/// and a silence detector that fires on that alone would rebuild the tap every
/// eight seconds of every quiet meeting — crying wolf on every muted
/// participant while shredding the recording into gaps.
#[test]
fn a_genuinely_quiet_meeting_is_never_a_fault() {
    let mut wd = Watchdog::new(macos_config());
    let mut now = 0u64;
    wd.arm(now);

    let mut activity = TapActivity::default();
    // Ten minutes of perfect digital silence with nothing playing anywhere.
    for _ in 0..6_000 {
        now += TICK.as_nanos() as u64;
        activity.buffers += 1;
        activity.silent_buffers += 1;
        activity.frames += 4_800;
        let verdict = wd.poll(now, activity, &OutputActivity::Idle);
        assert!(
            !matches!(verdict, Verdict::Faulted(_)),
            "a quiet room must never be a fault, got {verdict:?} at {now} ns"
        );
    }
}

/// ...and the same silence *with* corroboration is the macOS 26 defect.
#[test]
fn silence_while_something_is_playing_is_the_documented_stall() {
    let cfg = macos_config();
    let timeout = cfg.silence_timeout.as_nanos() as u64;
    let mut wd = Watchdog::new(cfg);
    wd.arm(0);

    let mut now = 0u64;
    let mut activity = TapActivity::default();
    let mut faulted_at = None;
    for _ in 0..1_000 {
        now += TICK.as_nanos() as u64;
        activity.buffers += 1;
        activity.silent_buffers += 1;
        activity.frames += 4_800;
        if let Verdict::Faulted(fault) = wd.poll(now, activity, &OutputActivity::Active) {
            let Fault::SilentWhileOutputRunning {
                silent_since_ns,
                corroborated_for_ns,
            } = fault
            else {
                panic!("expected a silence fault, got {fault:?}");
            };
            // The run opens at the poll *before* the first all-silent window,
            // so the gap covers the silent samples rather than only the part
            // observed after the evidence became conclusive. One poll interval
            // is the resolution of that claim and all it may cost.
            assert!(
                silent_since_ns <= TICK.as_nanos() as u64,
                "the gap must cover the silence from its start, not from the \
                 point it became provable; got {silent_since_ns} ns"
            );
            assert!(corroborated_for_ns >= timeout);
            faulted_at = Some(now);
            break;
        }
    }

    let faulted_at =
        faulted_at.expect("an 8 s bit-exact-zero stretch under live output must fault");
    assert!(
        faulted_at >= timeout,
        "fired early: {faulted_at} ns < {timeout} ns"
    );
    assert!(
        faulted_at < timeout + 2 * TICK.as_nanos() as u64,
        "fired late: {faulted_at} ns, expected within a poll of {timeout} ns"
    );
}

/// Corroboration has to hold for the *whole* stretch. An app that stops
/// playing half way through breaks the chain of evidence, and the clock starts
/// again rather than carrying on from where it was.
#[test]
fn corroborated_silence_must_be_continuous_to_count() {
    let cfg = macos_config();
    let timeout = cfg.silence_timeout.as_nanos() as u64;
    let mut wd = Watchdog::new(cfg);
    wd.arm(0);

    let mut now = 0u64;
    let mut activity = TapActivity::default();
    let tick = |wd: &mut Watchdog, now: &mut u64, activity: &mut TapActivity, out| {
        *now += TICK.as_nanos() as u64;
        activity.buffers += 1;
        activity.silent_buffers += 1;
        activity.frames += 4_800;
        wd.poll(*now, *activity, &out)
    };

    let ticks_to_threshold = (timeout / TICK.as_nanos() as u64) as usize;

    // Silence right up to the threshold: nearly there.
    for _ in 0..ticks_to_threshold - 1 {
        assert!(!matches!(
            tick(&mut wd, &mut now, &mut activity, OutputActivity::Active),
            Verdict::Faulted(_)
        ));
    }
    // The far end stops playing for one poll. Nothing is wrong any more.
    tick(&mut wd, &mut now, &mut activity, OutputActivity::Idle);

    // Two thirds of the threshold again. An implementation that accumulated
    // across the lapse instead of restarting would be well past it by the end
    // of this loop and would fire.
    for _ in 0..(ticks_to_threshold * 2 / 3) {
        let verdict = tick(&mut wd, &mut now, &mut activity, OutputActivity::Active);
        assert!(
            !matches!(verdict, Verdict::Faulted(_)),
            "the corroboration clock restarted, so 2 s cannot be enough"
        );
    }
    assert!(
        now > timeout,
        "the test never ran past the threshold, so it proves nothing"
    );
}

/// One audible sample is proof the tap is alive. It clears both clocks.
#[test]
fn a_single_audible_buffer_clears_the_silence_clock() {
    let cfg = macos_config();
    let timeout = cfg.silence_timeout.as_nanos() as u64;
    let mut wd = Watchdog::new(cfg);
    wd.arm(0);

    let mut now = 0u64;
    let mut activity = TapActivity::default();
    // One audible buffer just inside the threshold, over and over, for three
    // times as long as the threshold.
    let cadence = (timeout / TICK.as_nanos() as u64) as usize - 10;
    for i in 0..(cadence * 3) {
        now += TICK.as_nanos() as u64;
        activity.buffers += 1;
        activity.frames += 4_800;
        if i % cadence != cadence - 1 {
            activity.silent_buffers += 1;
        }
        let verdict = wd.poll(now, activity, &OutputActivity::Active);
        assert!(
            !matches!(verdict, Verdict::Faulted(_)),
            "audio arrived less than {timeout} ns ago; that is not a stall"
        );
    }
}

/// **Measured on real hardware while building this, and the reason the
/// threshold is not the 8 s the spec asks for.**
///
/// `kAudioProcessPropertyIsRunningOutput` means "this process has an output
/// stream open", not "this process is making a sound". An idle RustDesk in the
/// menu bar reports it continuously; so does any meeting app between
/// utterances, for the whole call. So "8 s of zeros while some process reports
/// running output" describes an ordinary pause in an ordinary meeting, and an
/// implementation that took the spec literally would rebuild the tap every
/// eight seconds of every call.
#[test]
fn an_ordinary_pause_under_an_app_holding_an_output_stream_is_not_a_stall() {
    let cfg = macos_config();
    assert!(
        cfg.silence_timeout >= Duration::from_secs(20),
        "a threshold this low cannot survive a meeting: the corroboration \
         signal is satisfied by any app with an output stream open"
    );

    let mut wd = Watchdog::new(cfg);
    wd.arm(0);
    let mut now = 0u64;
    let mut activity = TapActivity::default();

    // Fifteen seconds where nobody says anything, while the meeting app sits
    // there with its output unit running — which is what it always does.
    for _ in 0..150 {
        now += TICK.as_nanos() as u64;
        activity.buffers += 1;
        activity.silent_buffers += 1;
        activity.frames += 4_800;
        let verdict = wd.poll(now, activity, &OutputActivity::Active);
        assert!(
            !matches!(verdict, Verdict::Faulted(_)),
            "cried wolf on a {:.0} s pause",
            now as f64 / 1e9
        );
    }
}

/// Starvation outranks silence: if nothing is arriving at all there is nothing
/// to measure the silence of, and the two faults call for the same rebuild but
/// carry different gap semantics.
#[test]
fn starvation_outranks_silence() {
    let cfg = macos_config();
    let mut wd = Watchdog::new(cfg);
    wd.arm(0);

    // Build up a long silent stretch first...
    let mut now = 0u64;
    let mut activity = TapActivity::default();
    for _ in 0..70 {
        now += TICK.as_nanos() as u64;
        activity.buffers += 1;
        activity.silent_buffers += 1;
        activity.frames += 4_800;
        wd.poll(now, activity, &OutputActivity::Active);
    }
    // ...then stop delivering entirely.
    now += cfg.no_buffer_timeout.as_nanos() as u64 + cfg.silence_timeout.as_nanos() as u64;
    let verdict = wd.poll(now, activity, &OutputActivity::Active);
    assert!(
        matches!(verdict, Verdict::Faulted(Fault::NoBuffers { .. })),
        "expected NoBuffers to win, got {verdict:?}"
    );
}

/// §6.3's persistent banner: silence we cannot corroborate is still worth
/// telling the user about, because the commonest cause is a denied
/// system-audio grant, which is delivered as silence and never as an error.
#[test]
fn uncorroborated_silence_becomes_a_notice_not_a_rebuild() {
    let cfg = macos_config();
    let notice = cfg.quiet_notice_after.as_nanos() as u64;
    let mut wd = Watchdog::new(cfg);
    wd.arm(0);

    let mut now = 0u64;
    let mut activity = TapActivity::default();
    let mut first_notice = None;
    for _ in 0..1_000 {
        now += TICK.as_nanos() as u64;
        activity.buffers += 1;
        activity.silent_buffers += 1;
        activity.frames += 4_800;
        match wd.poll(now, activity, &OutputActivity::Unknown) {
            Verdict::NoAudio { for_ns } => {
                assert!(for_ns >= notice);
                first_notice.get_or_insert(now);
            }
            Verdict::Faulted(f) => panic!("uncorroborated silence must never rebuild: {f:?}"),
            Verdict::Healthy => assert!(first_notice.is_none()),
        }
    }
    let first = first_notice.expect("30 s of silence must raise the banner");
    assert!(first >= notice && first < notice + 2 * TICK.as_nanos() as u64);
}

/// Asking the platform whether anything is playing is not free — on macOS it
/// walks the process-object list and reads a property per process — so it must
/// not happen on the healthy path.
#[test]
fn the_output_probe_is_not_consulted_while_audio_is_flowing() {
    let probe = CountingProbe::new(OutputActivity::Active);
    let mut wd = Watchdog::new(macos_config());
    wd.arm(0);

    let mut now = 0u64;
    let mut activity = TapActivity::default();
    for _ in 0..600 {
        now += TICK.as_nanos() as u64;
        activity.buffers += 1;
        activity.frames += 4_800;
        wd.poll(now, activity, &probe);
    }
    assert_eq!(
        probe.calls(),
        0,
        "a healthy tap must not cost a process-list walk every poll"
    );
}

/// The counters are shared with the sink, and a caller that hands the rebuilt
/// tap a *fresh* set of counters makes them go backwards. That must read as a
/// restart, not as an arithmetic underflow into a 584-year-long stall.
#[test]
fn counters_going_backwards_are_treated_as_a_restart() {
    let mut wd = Watchdog::new(macos_config());
    wd.arm(0);

    let mut now = TICK.as_nanos() as u64;
    wd.poll(
        now,
        TapActivity {
            buffers: 900,
            silent_buffers: 0,
            frames: 4_320_000,
        },
        &OutputActivity::Active,
    );

    now += TICK.as_nanos() as u64;
    let verdict = wd.poll(
        now,
        TapActivity {
            buffers: 1,
            silent_buffers: 0,
            frames: 4_800,
        },
        &OutputActivity::Active,
    );
    assert_eq!(verdict, Verdict::Healthy);
}

/// The silence clocks have to be cleared by an arm as well, and that is a
/// separate assertion from the starvation one because they are separate
/// fields. If a rebuilt tap inherited the dead one's silence run it would
/// fault again on the *next poll* rather than after another full threshold —
/// a rebuild every hundred milliseconds instead of every thirty seconds, which
/// is a rebuild storm dressed up as a working watchdog.
#[test]
fn arming_clears_a_standing_silence_fault_too() {
    let cfg = macos_config();
    let mut wd = Watchdog::new(cfg);
    wd.arm(0);

    let mut now = 0u64;
    let mut activity = TapActivity::default();
    let mut faulted_at = None;
    for _ in 0..1_000 {
        now += TICK.as_nanos() as u64;
        activity.buffers += 1;
        activity.silent_buffers += 1;
        activity.frames += 4_800;
        if matches!(
            wd.poll(now, activity, &OutputActivity::Active),
            Verdict::Faulted(_)
        ) {
            faulted_at = Some(now);
            break;
        }
    }
    let faulted_at = faulted_at.expect("the silence rule never fired");

    // This is what the supervisor does after a successful rebuild.
    wd.arm(now);

    // The silence continues, because the rebuild did not fix anything. It must
    // still take a full threshold to say so again.
    let mut refaulted_at = None;
    for _ in 0..1_000 {
        now += TICK.as_nanos() as u64;
        activity.buffers += 1;
        activity.silent_buffers += 1;
        activity.frames += 4_800;
        if matches!(
            wd.poll(now, activity, &OutputActivity::Active),
            Verdict::Faulted(_)
        ) {
            refaulted_at = Some(now);
            break;
        }
    }
    let refaulted_at = refaulted_at.expect("the rule stopped working after an arm");
    assert!(
        refaulted_at - faulted_at >= cfg.silence_timeout.as_nanos() as u64,
        "the second fault came {} ns after the first; the arm did not clear \
         the silence clocks",
        refaulted_at - faulted_at
    );
}

/// Re-arming is what the supervisor does after a rebuild, and it has to clear
/// every clock or the freshly built tap inherits the dead one's fault.
#[test]
fn arming_clears_a_standing_fault() {
    let cfg = macos_config();
    let timeout = cfg.no_buffer_timeout.as_nanos() as u64;
    let mut wd = Watchdog::new(cfg);
    wd.arm(0);

    let at = timeout + 1;
    assert!(matches!(
        wd.poll(at, TapActivity::default(), &OutputActivity::Active),
        Verdict::Faulted(_)
    ));

    wd.arm(at);
    assert_eq!(
        wd.poll(at + 1, TapActivity::default(), &OutputActivity::Active),
        Verdict::Healthy
    );
}

// ------------------------------------------------------- device-change signal

/// The listener block runs on a Core Audio dispatch queue. It may not block
/// and may not allocate, so raising a signal is two atomic RMWs and nothing
/// else. (`tests/no_alloc_signal.rs` proves the allocation half.)
#[test]
fn device_change_signals_coalesce_and_are_cleared_by_taking_them() {
    let signal = DeviceChangeSignal::new();
    assert!(signal.take().is_empty());

    signal.raise(DeviceChangeKind::DefaultOutput);
    signal.raise(DeviceChangeKind::DefaultOutput);
    signal.raise(DeviceChangeKind::DeviceList);

    let taken = signal.take();
    assert!(taken.contains(DeviceChangeKind::DefaultOutput));
    assert!(taken.contains(DeviceChangeKind::DeviceList));
    assert!(!taken.contains(DeviceChangeKind::DefaultInput));
    assert_eq!(
        signal.raises(),
        3,
        "every raise is counted even though they coalesce into one set"
    );

    assert!(
        signal.take().is_empty(),
        "taking must clear, or one AirPods connect rebuilds forever"
    );
}

// -------------------------------------------------------------- supervisor

/// Everything a supervisor test needs: a clock, a log of what happened to the
/// taps, and a factory that can be told to fail.
struct Harness {
    clock: Arc<ManualClock>,
    log: TapLog,
    opens: Arc<AtomicUsize>,
    fail_opens: Arc<AtomicUsize>,
    sink: SinkHandle,
}

impl Harness {
    fn new() -> Self {
        Self {
            clock: ManualClock::new(),
            log: TapLog::new(),
            opens: Arc::new(AtomicUsize::new(0)),
            fail_opens: Arc::new(AtomicUsize::new(0)),
            sink: SinkHandle::new(),
        }
    }

    /// Make the next `n` `open()` calls fail.
    fn fail_next_opens(&self, n: usize) {
        self.fail_opens.store(n, Ordering::Relaxed);
    }

    fn supervisor(&self, cfg: SupervisorConfig) -> CaptureSupervisor {
        self.supervisor_with_formats(cfg, vec![f48()])
    }

    fn supervisor_with_formats(
        &self,
        cfg: SupervisorConfig,
        formats: Vec<StreamFormat>,
    ) -> CaptureSupervisor {
        let log = self.log.clone();
        let opens = Arc::clone(&self.opens);
        let fail = Arc::clone(&self.fail_opens);
        let sink = self.sink.clone();
        CaptureSupervisor::new(
            cfg,
            Arc::clone(&self.clock) as Arc<dyn fotw_audio::clock::Clock>,
            move || {
                let n = opens.fetch_add(1, Ordering::Relaxed);
                if fail.load(Ordering::Relaxed) > 0 {
                    fail.fetch_sub(1, Ordering::Relaxed);
                    return Err(TapError::platform("AudioHardwareCreateProcessTap failed"));
                }
                let format = formats.get(n).copied().unwrap_or_else(|| formats[0]);
                Ok(Box::new(
                    FakeTap::new(TapId::system_default(), format)
                        .with_start_formats([format])
                        .with_log(log.clone()),
                ) as Box<dyn AudioTap>)
            },
            move || sink.sink(),
        )
    }

    fn advance(&self, d: Duration) {
        self.clock.advance(d);
    }
}

fn supervisor_config() -> SupervisorConfig {
    SupervisorConfig::default()
}

/// Issue #25's "recover with the FULL sequence, in order". Partial recovery
/// (IOProc-only, aggregate-only) is documented as unreliable, so the old tap
/// must be stopped *and dropped* — which on macOS is what destroys the
/// aggregate and then the tap — before a new one is created.
#[test]
fn a_stalled_tap_is_destroyed_before_the_replacement_is_created() {
    let h = Harness::new();
    let cfg = supervisor_config();
    let mut sup = h.supervisor(cfg.clone());
    sup.start().unwrap();
    h.log.clear();

    // Nothing arrives, ever.
    h.advance(cfg.watchdog.no_buffer_timeout + TICK);
    let outcome = sup.poll(TapActivity::default(), &OutputActivity::Active);
    assert_eq!(outcome, PollOutcome::Rebuilt);

    assert_eq!(
        h.log.events(),
        vec![
            TapEvent::Stopped,
            TapEvent::Dropped,
            TapEvent::Opened(TapId::system_default()),
            TapEvent::Started(f48()),
        ],
        "teardown must complete before the rebuild begins"
    );
    assert_eq!(sup.rebuilds(), 1);
}

/// A starved tap wrote nothing at all for the outage, so the recording has a
/// *hole*. Concatenating across it moves every later timestamp earlier by the
/// length of the outage — note anchors, STT offsets and the whole two-stream
/// alignment. The gap therefore carries the frame count the writer has to
/// insert to keep byte offset and session time the same number.
#[test]
fn a_starvation_gap_is_reported_as_unwritten_and_must_be_padded() {
    let h = Harness::new();
    let cfg = supervisor_config();
    let mut sup = h.supervisor(cfg.clone());
    sup.start().unwrap();

    // One good second of audio. Two polls: the first only establishes the
    // baseline the deltas are taken against — the counters are shared with a
    // sink that has been running since before this supervisor existed, so
    // their absolute values say nothing on their own.
    let mut activity = TapActivity {
        buffers: 5,
        silent_buffers: 0,
        frames: 24_000,
    };
    h.advance(Duration::from_millis(500));
    assert_eq!(
        sup.poll(activity, &OutputActivity::Active),
        PollOutcome::Healthy
    );
    h.advance(Duration::from_millis(500));
    activity.buffers += 5;
    activity.frames += 24_000;
    assert_eq!(
        sup.poll(activity, &OutputActivity::Active),
        PollOutcome::Healthy
    );

    // Then the tap goes dead for exactly the timeout.
    h.advance(cfg.watchdog.no_buffer_timeout);
    assert_eq!(
        sup.poll(activity, &OutputActivity::Active),
        PollOutcome::Rebuilt
    );

    let events = sup.drain_events();
    let gaps: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            HealthEvent::Recovered { gaps, .. } => Some(gaps.clone()),
            _ => None,
        })
        .flatten()
        .collect();

    assert_eq!(gaps.len(), 1, "one hole, one gap: {gaps:?}");
    let gap = &gaps[0];
    assert_eq!(gap.kind, GapKind::Unwritten);
    assert_eq!(gap.reason, GapReason::TapStalledNoBuffers);
    assert_eq!(
        gap.start_ns,
        Duration::from_secs(1).as_nanos() as u64,
        "the gap opens at the last buffer that actually arrived"
    );
    assert_eq!(
        gap.duration_ns(),
        cfg.watchdog.no_buffer_timeout.as_nanos() as u64
    );
    assert_eq!(
        gap.frames_to_pad(48_000),
        48_000 * cfg.watchdog.no_buffer_timeout.as_secs(),
        "the writer must insert exactly this many frames of silence"
    );

    // And the audio that was already captured is untouched: recovery never
    // rewinds, truncates or reopens anything downstream.
    activity.buffers += 1;
    activity.frames += 4_800;
    h.advance(TICK);
    assert_eq!(
        sup.poll(activity, &OutputActivity::Active),
        PollOutcome::Healthy
    );
}

/// The other fault produces the other kind of gap, and confusing them
/// corrupts the timeline in the opposite direction. During a *silent* stall
/// the IOProc kept firing and zero samples were written, so those seconds are
/// present in the file: padding them again would push everything after the
/// stall 8 seconds late. Only the rebuild window itself is unwritten.
#[test]
fn a_silent_stall_reports_captured_silence_and_the_rebuild_window_separately() {
    let h = Harness::new();
    let cfg = supervisor_config();
    let mut sup = h.supervisor(cfg.clone());
    sup.start().unwrap();

    let mut activity = TapActivity::default();
    let mut outcome = PollOutcome::Healthy;
    for _ in 0..1_000 {
        h.advance(TICK);
        activity.buffers += 1;
        activity.silent_buffers += 1;
        activity.frames += 4_800;
        outcome = sup.poll(activity, &OutputActivity::Active);
        if outcome == PollOutcome::Rebuilt {
            break;
        }
    }
    assert_eq!(outcome, PollOutcome::Rebuilt);

    let gaps: Vec<_> = sup
        .drain_events()
        .iter()
        .filter_map(|e| match e {
            HealthEvent::Recovered { gaps, .. } => Some(gaps.clone()),
            _ => None,
        })
        .flatten()
        .collect();

    assert_eq!(
        gaps.len(),
        2,
        "captured-silence and rebuild-window: {gaps:?}"
    );

    let silent = &gaps[0];
    assert_eq!(silent.kind, GapKind::Silent);
    assert_eq!(silent.reason, GapReason::TapStalledSilent);
    assert!(
        silent.start_ns <= TICK.as_nanos() as u64,
        "every silent sample is in the gap"
    );
    assert!(silent.duration_ns() >= cfg.watchdog.silence_timeout.as_nanos() as u64);
    assert_eq!(
        silent.frames_to_pad(48_000),
        0,
        "those samples are already in the file; padding them would shift the \
         rest of the meeting later by the length of the stall"
    );

    let window = &gaps[1];
    assert_eq!(window.kind, GapKind::Unwritten);
    assert_eq!(window.reason, GapReason::RebuildWindow);
    assert_eq!(
        window.start_ns, silent.end_ns,
        "the two gaps must abut, or the timeline has a hole between them"
    );
}

/// "A silent auto-recovery that loses 30 seconds is better than losing
/// everything, but the user still needs to know the recording had a gap."
#[test]
fn the_user_is_told_what_was_lost() {
    let h = Harness::new();
    let cfg = supervisor_config();
    let mut sup = h.supervisor(cfg.clone());
    sup.start().unwrap();

    let mut activity = TapActivity {
        buffers: 5,
        silent_buffers: 0,
        frames: 24_000,
    };
    h.advance(Duration::from_millis(500));
    sup.poll(activity, &OutputActivity::Active);
    h.advance(Duration::from_millis(500));
    activity.buffers += 5;
    activity.frames += 24_000;
    sup.poll(activity, &OutputActivity::Active);

    h.advance(cfg.watchdog.no_buffer_timeout);
    sup.poll(activity, &OutputActivity::Active);

    let events = sup.drain_events();
    assert!(
        events
            .iter()
            .any(|e| matches!(e, HealthEvent::StallDetected { .. })),
        "the stall itself is logged for support: {events:?}"
    );

    let told: Vec<String> = events
        .iter()
        .filter(|e| e.is_user_visible())
        .map(HealthEvent::user_message)
        .collect();
    assert!(
        !told.is_empty(),
        "a recovered gap the user is never told about \
         is indistinguishable from data we quietly lost"
    );
    let message = told.join(" | ");
    assert!(
        message.contains("5.0 s") || message.contains("5 s"),
        "the message must name the length of the gap, got {message:?}"
    );

    assert!(
        sup.drain_events().is_empty(),
        "draining twice must not re-notify the user"
    );
}

/// A rebuild that fails must not spin. It backs off, keeps the gap open, and
/// tries again — and the gap that is eventually reported covers the whole
/// outage including every failed attempt.
#[test]
fn a_failed_rebuild_backs_off_and_the_gap_keeps_growing() {
    let h = Harness::new();
    let cfg = SupervisorConfig {
        retry_backoff: Duration::from_millis(250),
        ..supervisor_config()
    };
    let mut sup = h.supervisor(cfg.clone());
    sup.start().unwrap();
    h.fail_next_opens(2);

    h.advance(cfg.watchdog.no_buffer_timeout);
    assert_eq!(
        sup.poll(TapActivity::default(), &OutputActivity::Active),
        PollOutcome::Retrying
    );
    // Immediately polling again must not hammer the HAL.
    assert_eq!(
        sup.poll(TapActivity::default(), &OutputActivity::Active),
        PollOutcome::Waiting
    );
    let opens_after_first = h.opens.load(Ordering::Relaxed);

    h.advance(Duration::from_millis(250));
    assert_eq!(
        sup.poll(TapActivity::default(), &OutputActivity::Active),
        PollOutcome::Retrying
    );
    assert_eq!(h.opens.load(Ordering::Relaxed), opens_after_first + 1);

    // Third attempt succeeds. The backoff is exponential, so wait long enough.
    h.advance(Duration::from_secs(5));
    assert_eq!(
        sup.poll(TapActivity::default(), &OutputActivity::Active),
        PollOutcome::Rebuilt
    );

    let gaps: Vec<_> = sup
        .drain_events()
        .iter()
        .filter_map(|e| match e {
            HealthEvent::Recovered { gaps, .. } => Some(gaps.clone()),
            _ => None,
        })
        .flatten()
        .collect();
    assert_eq!(gaps.len(), 1);
    assert_eq!(
        gaps[0].start_ns, 0,
        "the outage began when the tap stopped, not when the last retry did"
    );
    assert_eq!(
        gaps[0].duration_ns(),
        h.clock.now_ns(),
        "every failed attempt is inside the gap"
    );

    let failures = sup
        .drain_events()
        .iter()
        .filter(|e| matches!(e, HealthEvent::RecoveryFailed { .. }))
        .count();
    assert_eq!(failures, 0, "events are drained once, not replayed");
}

/// The scenario that makes an unbounded retry loop actively harmful: a denied
/// system-audio grant delivers permanent silence and no error, so a watchdog
/// with no ceiling would tear the tap down every eight seconds for the whole
/// meeting and turn a bad recording into a shredded one.
#[test]
fn recovery_is_abandoned_after_the_attempt_ceiling_and_says_so_once() {
    let h = Harness::new();
    let cfg = SupervisorConfig {
        max_attempts: 3,
        retry_backoff: Duration::from_millis(100),
        ..supervisor_config()
    };
    let mut sup = h.supervisor(cfg.clone());
    sup.start().unwrap();
    h.fail_next_opens(100);

    h.advance(cfg.watchdog.no_buffer_timeout);
    let mut outcomes = Vec::new();
    for _ in 0..40 {
        outcomes.push(sup.poll(TapActivity::default(), &OutputActivity::Active));
        h.advance(Duration::from_secs(2));
    }

    assert!(
        outcomes.contains(&PollOutcome::Abandoned),
        "expected the supervisor to give up: {outcomes:?}"
    );
    assert_eq!(
        h.opens.load(Ordering::Relaxed),
        1 + cfg.max_attempts as usize,
        "the initial open plus exactly max_attempts rebuild attempts"
    );

    let events = sup.drain_events();
    let gave_up = events
        .iter()
        .filter(|e| matches!(e, HealthEvent::GaveUp { .. }))
        .count();
    assert_eq!(gave_up, 1, "told once, not once per poll: {events:?}");
    assert!(events.iter().any(HealthEvent::is_user_visible));
}

/// Issue #26: AirPods connecting fires several notifications in a burst. They
/// must produce exactly one rebuild, 300 ms after the burst goes quiet.
#[test]
fn a_burst_of_device_changes_produces_exactly_one_debounced_rebuild() {
    let h = Harness::new();
    let cfg = supervisor_config();
    let mut sup = h.supervisor(cfg.clone());
    sup.start().unwrap();
    let signal = sup.signal();

    let activity = TapActivity {
        buffers: 1,
        silent_buffers: 0,
        frames: 4_800,
    };

    for _ in 0..5 {
        signal.raise(DeviceChangeKind::DefaultOutput);
        signal.raise(DeviceChangeKind::DeviceList);
        h.advance(Duration::from_millis(20));
        assert_eq!(
            sup.poll(activity, &OutputActivity::Active),
            PollOutcome::Waiting,
            "still inside the debounce window"
        );
    }

    h.advance(cfg.device_change_debounce - Duration::from_millis(1));
    assert_eq!(
        sup.poll(activity, &OutputActivity::Active),
        PollOutcome::Waiting
    );

    h.advance(Duration::from_millis(2));
    assert_eq!(
        sup.poll(activity, &OutputActivity::Active),
        PollOutcome::Rebuilt
    );
    assert_eq!(sup.rebuilds(), 1, "one switch, one rebuild");

    // And the burst is spent: no second rebuild trails behind it.
    for _ in 0..10 {
        h.advance(Duration::from_millis(200));
        assert_ne!(
            sup.poll(activity, &OutputActivity::Active),
            PollOutcome::Rebuilt
        );
    }

    let events = sup.drain_events();
    assert!(
        events
            .iter()
            .any(|e| matches!(e, HealthEvent::DeviceChanged { .. })),
        "{events:?}"
    );
    let gaps: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            HealthEvent::Recovered { gaps, .. } => Some(gaps.clone()),
            _ => None,
        })
        .flatten()
        .collect();
    assert_eq!(gaps.len(), 1);
    assert_eq!(gaps[0].kind, GapKind::Unwritten);
    assert_eq!(gaps[0].reason, GapReason::DeviceChanged);
}

/// **Found by running this on a real Mac, and it shredded a 45-second
/// recording into 27 rebuilds.**
///
/// Tearing down and rebuilding the tap destroys and recreates an aggregate
/// device, and that changes `kAudioHardwarePropertyDevices` — which is one of
/// the properties issue #26 says to listen to. Our own listener sees our own
/// rebuild, calls it a device change, and rebuilds again. Forever.
/// (`kAudioAggregateDeviceIsPrivateKey` keeps the aggregate out of the user's
/// Sound settings; it does **not** keep it out of the device list.)
///
/// A device list changing is never a *necessary* rebuild trigger anyway: a
/// device that appears or disappears only matters to a live tap when it
/// becomes, or stops being, the default — and that raises
/// `kAudioHardwarePropertyDefaultOutputDevice` separately.
#[test]
fn a_device_list_change_on_its_own_never_rebuilds() {
    let h = Harness::new();
    let cfg = supervisor_config();
    let mut sup = h.supervisor(cfg.clone());
    sup.start().unwrap();

    let mut activity = TapActivity::default();
    for _ in 0..100 {
        // Exactly what our own rebuild looks like from the listener's side.
        sup.signal().raise(DeviceChangeKind::DeviceList);
        h.advance(TICK);
        activity.buffers += 1;
        activity.frames += 4_800;
        assert_eq!(
            sup.poll(activity, &OutputActivity::Active),
            PollOutcome::Healthy,
            "a device-list change is not a reason to spend a gap"
        );
    }
    assert_eq!(sup.rebuilds(), 0);

    // The change the aggregate rebuild does NOT cause still works.
    sup.signal().raise(DeviceChangeKind::DefaultOutput);
    sup.poll(activity, &OutputActivity::Active);
    h.advance(cfg.device_change_debounce + TICK);
    assert_eq!(
        sup.poll(activity, &OutputActivity::Active),
        PollOutcome::Rebuilt
    );
}

/// The structural backstop under the same failure. Whatever the cause, a
/// device change may not rebuild the tap again immediately after a rebuild:
/// if a rebuild can provoke the notification that triggers the next one, the
/// only thing that reliably breaks the cycle is a floor on the rate.
#[test]
fn a_device_change_cannot_rebuild_again_immediately_after_a_rebuild() {
    let h = Harness::new();
    let cfg = supervisor_config();
    let mut sup = h.supervisor(cfg.clone());
    sup.start().unwrap();

    let activity = TapActivity {
        buffers: 1,
        silent_buffers: 0,
        frames: 4_800,
    };

    // First switch: a real one, and it rebuilds.
    sup.signal().raise(DeviceChangeKind::DefaultOutput);
    sup.poll(activity, &OutputActivity::Active);
    h.advance(cfg.device_change_debounce + TICK);
    assert_eq!(
        sup.poll(activity, &OutputActivity::Active),
        PollOutcome::Rebuilt
    );

    // A second one arrives right behind it — which is what a rebuild that
    // provokes its own notification looks like.
    sup.signal().raise(DeviceChangeKind::DefaultOutput);
    sup.poll(activity, &OutputActivity::Active);
    h.advance(cfg.device_change_debounce + TICK);
    assert_ne!(
        sup.poll(activity, &OutputActivity::Active),
        PollOutcome::Rebuilt,
        "two rebuilds inside {:?} is a feedback loop, not two device changes",
        cfg.min_rebuild_interval
    );
    assert_eq!(sup.rebuilds(), 1);

    // Once the floor has passed, a genuine change is honoured again.
    h.advance(cfg.min_rebuild_interval);
    sup.signal().raise(DeviceChangeKind::DefaultOutput);
    sup.poll(activity, &OutputActivity::Active);
    h.advance(cfg.device_change_debounce + TICK);
    assert_eq!(
        sup.poll(activity, &OutputActivity::Active),
        PollOutcome::Rebuilt
    );
    assert_eq!(sup.rebuilds(), 2);
}

/// A device that chatters — a flaky dock, a Bluetooth link renegotiating —
/// must not postpone the rebuild indefinitely. The trailing-edge debounce has
/// a ceiling.
#[test]
fn a_chattering_device_cannot_postpone_the_rebuild_forever() {
    let h = Harness::new();
    let cfg = supervisor_config();
    let mut sup = h.supervisor(cfg.clone());
    sup.start().unwrap();
    let signal = sup.signal();

    let activity = TapActivity {
        buffers: 1,
        silent_buffers: 0,
        frames: 4_800,
    };

    let mut rebuilt_at = None;
    for i in 0..100u32 {
        signal.raise(DeviceChangeKind::DefaultOutput);
        h.advance(Duration::from_millis(100));
        if sup.poll(activity, &OutputActivity::Active) == PollOutcome::Rebuilt {
            rebuilt_at = Some(i);
            break;
        }
    }
    let at = rebuilt_at.expect("the debounce ceiling must eventually fire");
    assert!(
        h.clock.now_ns()
            <= (cfg.device_change_debounce_ceiling + Duration::from_millis(200)).as_nanos() as u64,
        "fired at tick {at} ({} ns), past the ceiling",
        h.clock.now_ns()
    );
}

/// A device change that arrives while the tap is *already* being rebuilt for
/// another reason has been answered by that rebuild. Acting on it again when
/// the debounce expires spends a second gap to arrive at the same place.
#[test]
fn a_device_change_answered_by_a_rebuild_does_not_cause_a_second_one() {
    let h = Harness::new();
    let cfg = supervisor_config();
    let mut sup = h.supervisor(cfg.clone());
    sup.start().unwrap();

    // The tap starves and the hardware moves at the same moment — a dock
    // coming loose does exactly this.
    h.advance(cfg.watchdog.no_buffer_timeout + TICK);
    sup.signal().raise(DeviceChangeKind::DefaultOutput);
    assert_eq!(
        sup.poll(TapActivity::default(), &OutputActivity::Active),
        PollOutcome::Rebuilt,
        "the fault outranks the pending change and rebuilds immediately"
    );
    assert_eq!(sup.rebuilds(), 1);

    let _ = sup.drain_events();

    // Audio is flowing again. The change that arrived mid-outage must not
    // surface at all once its debounce window elapses — not as a second
    // rebuild, and not as a device-change event either. Asserting on the event
    // and not only on the rebuild matters: the rebuild-rate floor would hide a
    // still-armed debounce, and two overlapping defences that are only ever
    // tested together are one defence.
    let mut activity = TapActivity::default();
    for _ in 0..40 {
        h.advance(TICK);
        activity.buffers += 1;
        activity.frames += 4_800;
        assert_ne!(
            sup.poll(activity, &OutputActivity::Active),
            PollOutcome::Rebuilt,
            "the rebuild already answered that change"
        );
    }
    assert_eq!(sup.rebuilds(), 1);

    let events = sup.drain_events();
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, HealthEvent::DeviceChanged { .. })),
        "the pending change outlived the rebuild that answered it: {events:?}"
    );
}

/// A Bluetooth headset engaging HFP changes the sample rate on both legs mid
/// meeting. The rebuilt tap's format is authoritative and must be handed to
/// the caller — a converter left configured for the old ASBD is the documented
/// way this turns into garbage audio rather than no audio.
#[test]
fn a_rebuild_reports_the_new_format() {
    let h = Harness::new();
    let cfg = supervisor_config();
    let mut sup = h.supervisor_with_formats(cfg.clone(), vec![f48(), f16()]);
    assert_eq!(sup.start().unwrap(), f48());
    assert_eq!(sup.format(), Some(f48()));

    sup.signal().raise(DeviceChangeKind::StreamFormat);
    // The debounce window is measured from the poll that *observes* the
    // change, not from the raise: the listener thread may not read a clock.
    assert_eq!(
        sup.poll(TapActivity::default(), &OutputActivity::Active),
        PollOutcome::Waiting
    );
    h.advance(cfg.device_change_debounce + TICK);
    assert_eq!(
        sup.poll(TapActivity::default(), &OutputActivity::Active),
        PollOutcome::Rebuilt
    );

    assert_eq!(sup.format(), Some(f16()));
    let formats: Vec<StreamFormat> = sup
        .drain_events()
        .iter()
        .filter_map(|e| match e {
            HealthEvent::Recovered { format, .. } => Some(*format),
            _ => None,
        })
        .collect();
    assert_eq!(formats, vec![f16()]);
}

/// A supervisor that rebuilds a healthy tap is worse than no supervisor: every
/// rebuild costs a gap.
#[test]
fn a_healthy_tap_is_never_rebuilt() {
    let h = Harness::new();
    let mut sup = h.supervisor(supervisor_config());
    sup.start().unwrap();
    h.log.clear();

    let mut activity = TapActivity::default();
    for _ in 0..6_000 {
        h.advance(TICK);
        activity.buffers += 1;
        activity.frames += 4_800;
        assert_eq!(
            sup.poll(activity, &OutputActivity::Active),
            PollOutcome::Healthy
        );
    }
    assert_eq!(sup.rebuilds(), 0);
    assert!(h.log.events().is_empty(), "{:?}", h.log.events());
    assert!(sup.drain_events().is_empty());
}

/// Once recovery has been abandoned, a device change is new evidence — the
/// user plugged something back in — and is worth another attempt. Without
/// this, unplugging a dock at minute three costs the remaining 42 minutes.
#[test]
fn a_device_change_re_arms_recovery_after_it_was_abandoned() {
    let h = Harness::new();
    let cfg = SupervisorConfig {
        max_attempts: 1,
        retry_backoff: Duration::from_millis(50),
        ..supervisor_config()
    };
    let mut sup = h.supervisor(cfg.clone());
    sup.start().unwrap();
    h.fail_next_opens(1);

    h.advance(cfg.watchdog.no_buffer_timeout);
    sup.poll(TapActivity::default(), &OutputActivity::Active);
    h.advance(Duration::from_secs(1));
    assert_eq!(
        sup.poll(TapActivity::default(), &OutputActivity::Active),
        PollOutcome::Abandoned
    );
    let _ = sup.drain_events();

    // Give up means give up: further polls do nothing at all.
    let opens = h.opens.load(Ordering::Relaxed);
    for _ in 0..10 {
        h.advance(Duration::from_secs(10));
        assert_eq!(
            sup.poll(TapActivity::default(), &OutputActivity::Active),
            PollOutcome::Abandoned
        );
    }
    assert_eq!(h.opens.load(Ordering::Relaxed), opens);

    // ...until the hardware changes underneath us.
    sup.signal().raise(DeviceChangeKind::DefaultOutput);
    assert_eq!(
        sup.poll(TapActivity::default(), &OutputActivity::Active),
        PollOutcome::Abandoned,
        "still inside the debounce window"
    );
    h.advance(cfg.device_change_debounce + TICK);
    assert_eq!(
        sup.poll(TapActivity::default(), &OutputActivity::Active),
        PollOutcome::Rebuilt
    );
    assert!(h.opens.load(Ordering::Relaxed) > opens);
}

/// The §6.0 correction says a tap-only aggregate removes default-output-change
/// tracking from the critical path; issue #26 says the tap must be destroyed
/// on every default-output change. They cannot both be right, so the choice is
/// configuration with a documented default rather than a buried assumption.
#[test]
fn device_changes_can_be_configured_to_re_arm_instead_of_rebuild() {
    let h = Harness::new();
    let cfg = SupervisorConfig {
        rebuild_on_device_change: false,
        ..supervisor_config()
    };
    let mut sup = h.supervisor(cfg.clone());
    sup.start().unwrap();

    let activity = TapActivity {
        buffers: 1,
        silent_buffers: 0,
        frames: 4_800,
    };
    sup.signal().raise(DeviceChangeKind::DefaultOutput);
    assert_eq!(
        sup.poll(activity, &OutputActivity::Active),
        PollOutcome::Waiting
    );
    h.advance(cfg.device_change_debounce + TICK);
    assert_eq!(
        sup.poll(activity, &OutputActivity::Active),
        PollOutcome::Healthy
    );
    assert_eq!(sup.rebuilds(), 0, "no gap is spent on a tap that survived");

    // The stall watchdog is still the backstop, and it now measures from the
    // device change rather than from the last buffer before it.
    h.advance(cfg.watchdog.no_buffer_timeout + TICK);
    assert_eq!(
        sup.poll(activity, &OutputActivity::Active),
        PollOutcome::Rebuilt
    );
}

/// The safety net under the silence rule. A rebuild that succeeds and brings
/// no audio back has achieved nothing, and doing it again on a timer for
/// forty-five minutes turns a bad recording into a shredded one — a gap and a
/// notification every thirty seconds. It has to stop by itself.
#[test]
fn rebuilds_that_never_bring_audio_back_stop_by_themselves() {
    let h = Harness::new();
    let cfg = supervisor_config();
    let mut sup = h.supervisor(cfg.clone());
    sup.start().unwrap();

    // Half an hour of a meeting that is either genuinely silent or has a
    // denied system-audio grant. The supervisor cannot tell the two apart, and
    // neither can the corroboration signal.
    let mut activity = TapActivity::default();
    let mut abandoned = 0usize;
    for _ in 0..18_000 {
        h.advance(TICK);
        activity.buffers += 1;
        activity.silent_buffers += 1;
        activity.frames += 4_800;
        if sup.poll(activity, &OutputActivity::Active) == PollOutcome::Abandoned {
            abandoned += 1;
        }
    }

    assert_eq!(
        sup.rebuilds(),
        cfg.max_ineffective_recoveries,
        "one rebuild per ineffective attempt and then no more"
    );
    assert!(abandoned > 0, "the supervisor never actually stopped");

    let events = sup.drain_events();
    let told = events
        .iter()
        .filter(|e| matches!(e, HealthEvent::RecoveryIneffective { .. }))
        .count();
    assert_eq!(told, 1, "told once, not once per fault: {events:?}");
    let message = events
        .iter()
        .find(|e| matches!(e, HealthEvent::RecoveryIneffective { .. }))
        .map(HealthEvent::user_message)
        .unwrap();
    assert!(
        message.contains("Screen & System Audio Recording"),
        "the likeliest cause is a denied grant, so say where to fix it: {message}"
    );
}

/// ...and it has to start again by itself, or one quiet stretch early in a
/// meeting disables the watchdog for the rest of it.
#[test]
fn audible_audio_releases_the_quarantine() {
    let h = Harness::new();
    let cfg = supervisor_config();
    let mut sup = h.supervisor(cfg.clone());
    sup.start().unwrap();

    let mut activity = TapActivity::default();
    let silence =
        |h: &Harness, sup: &mut CaptureSupervisor, activity: &mut TapActivity, ticks: usize| {
            let mut outcomes = Vec::new();
            for _ in 0..ticks {
                h.advance(TICK);
                activity.buffers += 1;
                activity.silent_buffers += 1;
                activity.frames += 4_800;
                outcomes.push(sup.poll(*activity, &OutputActivity::Active));
            }
            outcomes
        };

    // Quiet for long enough to exhaust the ineffective-rebuild budget.
    let outcomes = silence(&h, &mut sup, &mut activity, 18_000);
    assert!(outcomes.contains(&PollOutcome::Abandoned));
    assert!(
        sup.is_quarantined(),
        "the state has to be observable: a session in it is one whose \
         recording is probably silent"
    );
    let rebuilds_while_quiet = sup.rebuilds();
    let _ = sup.drain_events();

    // Somebody finally speaks.
    for _ in 0..10 {
        h.advance(TICK);
        activity.buffers += 1;
        activity.frames += 4_800;
        assert_eq!(
            sup.poll(activity, &OutputActivity::Active),
            PollOutcome::Healthy,
            "audible audio must end the quarantine immediately"
        );
    }
    assert!(
        !sup.is_quarantined(),
        "one audible buffer releases it; waiting for the next fault to do it \
         leaves the supervisor reporting a state it is not in"
    );

    // And the watchdog is doing its job again.
    let outcomes = silence(&h, &mut sup, &mut activity, 18_000);
    assert!(
        outcomes.contains(&PollOutcome::Rebuilt),
        "supervision never resumed, so a real stall later in the meeting \
         would go unnoticed"
    );
    assert!(sup.rebuilds() > rebuilds_while_quiet);
}

/// Stopping is not a fault. A supervisor that has been stopped must not keep
/// rebuilding a tap the user deliberately ended.
#[test]
fn a_stopped_supervisor_stops_supervising() {
    let h = Harness::new();
    let cfg = supervisor_config();
    let mut sup = h.supervisor(cfg.clone());
    sup.start().unwrap();
    sup.stop().unwrap();

    h.advance(cfg.watchdog.no_buffer_timeout * 10);
    assert_eq!(
        sup.poll(TapActivity::default(), &OutputActivity::Active),
        PollOutcome::Stopped
    );
    assert_eq!(sup.rebuilds(), 0);
    assert_eq!(h.opens.load(Ordering::Relaxed), 1);
}
