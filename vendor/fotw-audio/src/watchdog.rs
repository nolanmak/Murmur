//! CAP-05: detecting a tap that has died without saying so.
//!
//! A Core Audio process tap can stay "running" — no error, no failed callback,
//! `AudioDeviceStart` long since returned success — while delivering nothing
//! at all, or nothing but bit-exact zeros. The user records a 45-minute
//! meeting and gets an empty transcript. It is the worst failure mode in the
//! product because it is silent *and* unrecoverable after the fact, and the
//! only defence is to notice it while the meeting is still running.
//!
//! Nothing in this module names an operating system. It is a pure function of
//! `(now_ns, counters, corroboration)`: the clock is an argument, the counters
//! come from atomics bumped on the IOProc, and the corroboration comes from
//! the platform through [`OutputProbe`]. That is what makes it testable with
//! no audio device and no elapsed real time.
//!
//! # The two conditions are not the same condition
//!
//! **Starvation** — no buffers arriving — and **digital silence** — buffers
//! arriving full of zeros — look alike in a bug report and are completely
//! different signals.
//!
//! *Starvation* is nearly self-evidencing. Where a platform advertises
//! [`crate::PlatformCaps::emits_silence_when_idle`], the IOProc fires on a
//! fixed cadence whether or not anything is playing, so its stopping means the
//! device is gone. macOS is such a platform, which is why zero buffers there
//! needs no second opinion.
//!
//! *Silence* evidences almost nothing on its own. A meeting where nobody is
//! talking, everyone is muted, or the far end is on hold produces bit-exact
//! zero buffers for as long as it likes, and that is not a fault — it is a
//! quiet meeting. **A silence detector that fires on silence alone would tear
//! the tap down every eight seconds of every quiet meeting**, spending a gap
//! each time, which is a worse recording than the one it set out to protect.
//!
//! So silence only counts as a fault while something else on the machine is
//! demonstrably rendering output: docs/REQUIREMENTS.md 6.4 says "bit-exact
//! zero for > 8 s **while any process reports
//! `kAudioProcessPropertyIsRunningOutput`**", and the second half of that
//! sentence is the whole design. Where the platform cannot answer the
//! question, the answer is [`OutputActivity::Unknown`] and the silence rule is
//! *disabled*, not assumed.
//!
//! # The corroboration is weaker than the spec assumes — measured
//!
//! `kAudioProcessPropertyIsRunningOutput` is documented as "the process is
//! running IO and there is at least one active output **stream**". It does not
//! mean the process is producing sound. Measured on the development machine
//! while building this: an idle RustDesk, sitting in the menu bar producing
//! nothing audible, reports `IsRunningOutput == true` continuously. So does a
//! browser tab with a paused player, and so does any meeting app between
//! utterances — which is precisely the case that matters, because a meeting
//! app keeps its output unit running for the whole call and renders silence
//! between speakers.
//!
//! **Taken literally, then, the rule specified in issue #25 and 6.4 — 8 s of
//! zeros while any process reports running output — fires on every eight-second
//! pause in every meeting.** The corroboration removes the "nothing is playing
//! anywhere" case, which is worth having, but it does not distinguish "an app
//! is producing audio" from "an app has a stream open and is producing
//! silence", and no cheap Core Audio property does. There is no output level
//! meter to consult.
//!
//! Two things follow, and both are in this crate rather than in a comment:
//!
//! 1. **The threshold cannot be 8 s.** It is 30 s by default — see
//!    [`WatchdogConfig::silence_timeout`] for the derivation.
//! 2. **Firing has to be self-limiting.** Even at 30 s the rule will
//!    occasionally be wrong, so a rebuild that does not restore audible audio
//!    is counted, and after a few of them the supervisor stops rebuilding
//!    until audio actually returns. See
//!    [`crate::supervisor::SupervisorConfig::max_ineffective_recoveries`].
//!
//! # Why the corroboration has to be continuous
//!
//! The corroborated clock restarts whenever the evidence lapses, rather than
//! accumulating across gaps. A fault therefore requires one unbroken stretch
//! in which every observed buffer was silent *and* output was running at every
//! observation. That is deliberately conservative: it can miss a stall that
//! happens to straddle a pause in playback, and it cannot fire on a meeting
//! that merely went quiet for a while. Given the two error costs — a missed
//! stall loses part of a meeting, a false stall shreds a good one into gaps
//! and can loop — missing is the cheaper mistake.
//!
//! # And why silence still gets reported when it cannot be corroborated
//!
//! A denied system-audio grant is delivered as silence and never as an error
//! (docs/REQUIREMENTS.md 6.3), so uncorroborated silence is exactly what a
//! permission problem looks like. It cannot justify a rebuild — a rebuild will
//! not grant a permission — but it justifies telling the user, which is §6.3's
//! persistent "No audio detected for 30 s" banner. That is
//! [`Verdict::NoAudio`].

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use crate::permission::PlatformCaps;

/// How much a tap has delivered, sampled off the real-time path.
///
/// Every field is a monotonically increasing total, not a rate: the watchdog
/// works in differences between consecutive polls, so a caller may sample at
/// whatever cadence suits it. The fields are exactly what an IOProc can
/// maintain with three relaxed atomic adds and no branches worth mentioning.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TapActivity {
    /// Buffers delivered to the sink since the counters were created.
    pub buffers: u64,
    /// How many of those were bit-exact digital silence.
    pub silent_buffers: u64,
    /// Frames delivered. Separate from `buffers` because a tap that keeps
    /// firing with empty buffers is starving even though its callback count
    /// is climbing.
    pub frames: u64,
}

impl TapActivity {
    /// Construct a snapshot.
    #[must_use]
    pub const fn new(buffers: u64, silent_buffers: u64, frames: u64) -> Self {
        Self {
            buffers,
            silent_buffers,
            frames,
        }
    }
}

/// The counters a [`crate::FrameSink`] bumps on the audio thread.
///
/// Three relaxed `fetch_add`s: no allocation, no locking, no branch that can
/// call into the runtime. Shared with the supervisor through an `Arc`, and
/// deliberately *not* reset when a tap is rebuilt — a rebuilt tap's audio is
/// the same session's audio, and a counter that restarts at zero is
/// indistinguishable from a stall until the next poll.
#[derive(Debug, Default)]
pub struct ActivityCounters {
    buffers: AtomicU64,
    silent_buffers: AtomicU64,
    frames: AtomicU64,
}

impl ActivityCounters {
    /// Fresh counters, all zero.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Record one delivered buffer. Safe to call from a real-time thread.
    pub fn record(&self, frames: u64, silent: bool) {
        self.buffers.fetch_add(1, Ordering::Relaxed);
        self.frames.fetch_add(frames, Ordering::Relaxed);
        if silent {
            self.silent_buffers.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Read the counters.
    ///
    /// The three loads are not atomic *together*, so a snapshot taken while
    /// [`record`](Self::record) is running can see a buffer counted but its
    /// frames not yet added. That skew is at most one buffer and self-corrects
    /// on the next poll; making it consistent would cost a lock on the audio
    /// thread, which is the one thing that must never happen there.
    #[must_use]
    pub fn snapshot(&self) -> TapActivity {
        TapActivity {
            buffers: self.buffers.load(Ordering::Relaxed),
            silent_buffers: self.silent_buffers.load(Ordering::Relaxed),
            frames: self.frames.load(Ordering::Relaxed),
        }
    }
}

/// Whether anything on this machine is currently rendering audio output.
///
/// The corroborating evidence for the silence rule. On macOS it is
/// `kAudioProcessPropertyIsRunningOutput` over the process-object list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OutputActivity {
    /// At least one process is rendering output right now.
    Active,
    /// Nothing is rendering output. Silence is expected and is not a fault.
    Idle,
    /// The platform cannot answer.
    ///
    /// **Not the same as `Idle`.** `Idle` is an observation; this is the
    /// absence of one, and it disables the silence rule rather than deciding
    /// it either way.
    Unknown,
}

/// Something that can answer [`OutputActivity`].
///
/// A trait rather than a plain argument so the watchdog can decide *not* to
/// ask: on macOS an answer costs a `kAudioHardwarePropertyProcessObjectList`
/// walk plus a property read per process, and paying that on every poll of
/// every healthy meeting is a cost with no corresponding benefit.
pub trait OutputProbe {
    /// Is anything rendering output right now?
    fn output_activity(&self) -> OutputActivity;
}

impl OutputProbe for OutputActivity {
    fn output_activity(&self) -> Self {
        *self
    }
}

impl<T: OutputProbe + ?Sized> OutputProbe for &T {
    fn output_activity(&self) -> OutputActivity {
        (**self).output_activity()
    }
}

/// What the watchdog decided this poll.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Audio is flowing, or has flowed recently enough.
    Healthy,
    /// Silence that cannot be corroborated. **Never a reason to rebuild** —
    /// the commonest cause is a denied permission, which no rebuild fixes —
    /// but §6.3's banner is rendered from this.
    NoAudio {
        /// How long the silence has run.
        for_ns: u64,
    },
    /// The tap is broken and should be torn down and rebuilt.
    Faulted(Fault),
}

/// A tap that has stopped working while still claiming to run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fault {
    /// No frames have arrived for longer than the timeout.
    ///
    /// Nothing was written for this interval, so the recording has a hole in
    /// it: see [`crate::supervisor::GapKind::Unwritten`].
    NoBuffers {
        /// Host time of the last frame that arrived — or of the arm, if none
        /// ever did. The start of the hole.
        since_ns: u64,
        /// How long the starvation has lasted.
        for_ns: u64,
    },
    /// Buffers are arriving on cadence and every sample in them is exactly
    /// zero, while at least one process is rendering output.
    ///
    /// Samples *were* written for this interval — zeros — so the timeline is
    /// intact and only the content is lost: see
    /// [`crate::supervisor::GapKind::Silent`].
    SilentWhileOutputRunning {
        /// Host time of the first silent buffer in the run. This is the start
        /// of the lost audio, which is earlier than the point at which the
        /// evidence became conclusive.
        silent_since_ns: u64,
        /// How long silence and running output have coincided without a
        /// break. This is what is measured against the threshold.
        corroborated_for_ns: u64,
    },
}

impl Fault {
    /// When the lost audio starts.
    #[must_use]
    pub const fn since_ns(&self) -> u64 {
        match self {
            Self::NoBuffers { since_ns, .. } => *since_ns,
            Self::SilentWhileOutputRunning {
                silent_since_ns, ..
            } => *silent_since_ns,
        }
    }

    /// A stable identifier for logs, manifests and bug reports.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::NoBuffers { .. } => "no-buffers",
            Self::SilentWhileOutputRunning { .. } => "silent-while-output-running",
        }
    }
}

impl std::fmt::Display for Fault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoBuffers { for_ns, .. } => {
                write!(f, "the tap delivered no audio for {:.1} s", secs(*for_ns))
            }
            Self::SilentWhileOutputRunning {
                corroborated_for_ns,
                ..
            } => write!(
                f,
                "the tap delivered {:.1} s of digital silence while audio was playing",
                secs(*corroborated_for_ns)
            ),
        }
    }
}

fn secs(ns: u64) -> f64 {
    ns as f64 / 1e9
}

/// Thresholds for [`Watchdog`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WatchdogConfig {
    /// How long with no frames before starvation is declared.
    ///
    /// The IOProc fires every ~10 ms, so the default is five hundred missed
    /// callbacks: far beyond any scheduling hiccup, sleep/wake stutter or GC
    /// pause, and still inside issue #25's "recovers within 10 s" acceptance
    /// once the ~300 ms rebuild is added.
    pub no_buffer_timeout: Duration,
    /// How long bit-exact silence must coincide with running output.
    ///
    /// **Deliberately 30 s and not the 8 s docs/REQUIREMENTS.md 6.4 and issue
    /// #25 specify.** The corroboration signal those numbers rest on means
    /// "an output stream is open", not "sound is being produced" (see the
    /// module docs, and the idle RustDesk that demonstrated it), so at 8 s the
    /// rule fires on ordinary pauses in ordinary meetings.
    ///
    /// 30 s is chosen from both ends. The defect's own reported stall windows
    /// run from **53 seconds to 16+ minutes**, so anything below ~50 s catches
    /// every case that has ever been observed and waiting longer buys nothing.
    /// From the other end, a stretch in which *every* participant is silent
    /// and no system sound plays for half a minute is rare in a live meeting,
    /// and 30 s is also §6.3's existing "No audio detected" threshold, so the
    /// banner and the recovery agree with each other instead of contradicting.
    pub silence_timeout: Duration,
    /// How long uncorroborated silence runs before the user is told.
    ///
    /// §6.3's persistent banner: "No audio detected for 30 s."
    pub quiet_notice_after: Duration,
    /// Mirrors [`crate::PlatformCaps::emits_silence_when_idle`].
    ///
    /// When true, the platform delivers callbacks even in a silent room, so
    /// their absence is conclusive on its own. When false — Windows endpoint
    /// loopback delivers *nothing* while the endpoint is idle — the absence of
    /// callbacks is the normal state of a machine playing nothing, and
    /// starvation needs the same corroboration silence does.
    pub emits_silence_when_idle: bool,
}

impl Default for WatchdogConfig {
    fn default() -> Self {
        Self {
            no_buffer_timeout: Duration::from_secs(5),
            silence_timeout: Duration::from_secs(30),
            quiet_notice_after: Duration::from_secs(30),
            emits_silence_when_idle: true,
        }
    }
}

impl WatchdogConfig {
    /// The default thresholds, tuned to what a platform actually delivers.
    #[must_use]
    pub fn for_caps(caps: &PlatformCaps) -> Self {
        Self {
            emits_silence_when_idle: caps.emits_silence_when_idle,
            ..Self::default()
        }
    }
}

/// The stall detector.
///
/// Holds no clock and no platform handle: [`poll`](Self::poll) is a pure
/// transition on `(now_ns, activity, probe)`.
#[derive(Debug)]
pub struct Watchdog {
    config: WatchdogConfig,
    /// Counters as of the previous poll. `None` immediately after an arm, when
    /// there is nothing to difference against yet.
    baseline: Option<TapActivity>,
    /// When the current watch began — a start, or a rebuild.
    armed_at_ns: u64,
    /// Host time of the previous poll. Buffers counted this poll were
    /// delivered somewhere in `(last_poll_ns, now_ns]`, so this is the
    /// earliest defensible start for a run of silence — which matters,
    /// because that start is where the gap marker opens.
    last_poll_ns: u64,
    /// Host time of the last poll at which frames had advanced.
    last_frames_ns: u64,
    /// Start of the current run of all-silent buffers, corroborated or not.
    quiet_since_ns: Option<u64>,
    /// Start of the current *corroborated* run. Reset the moment the evidence
    /// lapses, which is what makes the rule require one unbroken stretch.
    corroborated_since_ns: Option<u64>,
    /// Whether any buffer with an audible sample in it has arrived since the
    /// last arm. The supervisor uses this to tell "the rebuild worked" from
    /// "the rebuild changed nothing", which is what stops it rebuilding
    /// forever against a cause no rebuild can fix.
    audible_since_arm: bool,
}

impl Watchdog {
    /// A watchdog that has not been armed yet.
    #[must_use]
    pub const fn new(config: WatchdogConfig) -> Self {
        Self {
            config,
            baseline: None,
            armed_at_ns: 0,
            last_poll_ns: 0,
            last_frames_ns: 0,
            quiet_since_ns: None,
            corroborated_since_ns: None,
            audible_since_arm: false,
        }
    }

    /// Start (or restart) the watch at `now_ns`.
    ///
    /// Called on every successful start and every successful rebuild. It has
    /// to clear every clock, or a freshly built tap inherits the dead one's
    /// fault and is torn down again before it has delivered a single buffer.
    pub const fn arm(&mut self, now_ns: u64) {
        self.baseline = None;
        self.armed_at_ns = now_ns;
        self.last_poll_ns = now_ns;
        self.last_frames_ns = now_ns;
        self.quiet_since_ns = None;
        self.corroborated_since_ns = None;
        self.audible_since_arm = false;
    }

    /// The thresholds in force.
    #[must_use]
    pub const fn config(&self) -> &WatchdogConfig {
        &self.config
    }

    /// Whether a buffer with a non-zero sample has arrived since the last
    /// [`arm`](Self::arm).
    ///
    /// This is the only evidence available that a rebuild actually achieved
    /// something. A rebuild after which this stays false did not help, and
    /// doing it again will not help either.
    #[must_use]
    pub const fn audible_since_arm(&self) -> bool {
        self.audible_since_arm
    }

    /// Observe the tap and decide.
    ///
    /// `probe` is consulted only on the paths that need corroboration, so a
    /// healthy meeting never pays for it.
    pub fn poll(
        &mut self,
        now_ns: u64,
        activity: TapActivity,
        probe: &impl OutputProbe,
    ) -> Verdict {
        let previous = self.baseline.replace(activity);
        let prev_poll_ns = self.last_poll_ns;
        self.last_poll_ns = now_ns;

        // A caller that hands a rebuilt tap fresh counters makes these go
        // backwards. Treat it as a restart: subtracting would underflow into a
        // 584-year delta and read as a permanent stall.
        let previous = match previous {
            Some(p) if p.frames <= activity.frames && p.buffers <= activity.buffers => p,
            Some(_) => {
                self.last_frames_ns = now_ns;
                self.quiet_since_ns = None;
                self.corroborated_since_ns = None;
                return Verdict::Healthy;
            }
            None => return self.starvation_verdict(now_ns, probe),
        };

        let new_frames = activity.frames - previous.frames;
        let new_buffers = activity.buffers - previous.buffers;
        let new_silent = activity
            .silent_buffers
            .saturating_sub(previous.silent_buffers);

        if new_frames > 0 {
            self.last_frames_ns = now_ns;
        }

        if new_buffers > 0 {
            if new_silent == new_buffers {
                // Every buffer since the last poll was bit-exact zero. The run
                // opens at the *previous* poll, not at this one: those buffers
                // were delivered across that interval, and the gap marker has
                // to cover the samples that were actually lost rather than
                // only the ones observed after the evidence became clear.
                self.quiet_since_ns.get_or_insert(prev_poll_ns);
            } else {
                // One audible sample is proof the tap is alive. Both clocks
                // clear, not just the corroborated one.
                self.quiet_since_ns = None;
                self.corroborated_since_ns = None;
                self.audible_since_arm = true;
            }
        }

        // Starvation outranks silence: with nothing arriving there is nothing
        // to measure the silence of, and the gap semantics differ.
        if let Verdict::Faulted(f) = self.starvation_verdict(now_ns, probe) {
            return Verdict::Faulted(f);
        }

        let Some(quiet_since) = self.quiet_since_ns else {
            self.corroborated_since_ns = None;
            return Verdict::Healthy;
        };

        match probe.output_activity() {
            OutputActivity::Active => {
                // Same interval as the silence run, so the two clocks describe
                // the same observation rather than differing by a poll.
                let since = *self.corroborated_since_ns.get_or_insert(prev_poll_ns);
                let corroborated_for_ns = now_ns - since;
                if corroborated_for_ns >= self.config.silence_timeout.as_nanos() as u64 {
                    return Verdict::Faulted(Fault::SilentWhileOutputRunning {
                        silent_since_ns: quiet_since,
                        corroborated_for_ns,
                    });
                }
            }
            OutputActivity::Idle | OutputActivity::Unknown => {
                // The evidence lapsed. Start again rather than accumulate:
                // eight seconds of silence spread across a pause in playback
                // is not eight seconds of a broken tap.
                self.corroborated_since_ns = None;
            }
        }

        let quiet_for = now_ns - quiet_since;
        if quiet_for >= self.config.quiet_notice_after.as_nanos() as u64 {
            return Verdict::NoAudio { for_ns: quiet_for };
        }
        Verdict::Healthy
    }

    /// The starvation half, shared by the first poll after an arm and every
    /// poll after that.
    fn starvation_verdict(&self, now_ns: u64, probe: &impl OutputProbe) -> Verdict {
        let since_ns = self.last_frames_ns.max(self.armed_at_ns);
        let for_ns = now_ns.saturating_sub(since_ns);
        if for_ns < self.config.no_buffer_timeout.as_nanos() as u64 {
            return Verdict::Healthy;
        }
        // On a platform that stops delivering while the endpoint is idle,
        // "nothing arrived" is what a machine playing nothing looks like.
        // Rebuilding on it would tear the tap down every time the user paused
        // their music.
        if !self.config.emits_silence_when_idle && probe.output_activity() != OutputActivity::Active
        {
            return Verdict::NoAudio { for_ns };
        }
        Verdict::Faulted(Fault::NoBuffers { since_ns, for_ns })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counters_are_a_running_total() {
        let c = ActivityCounters::new();
        assert_eq!(c.snapshot(), TapActivity::default());
        c.record(480, false);
        c.record(480, true);
        assert_eq!(c.snapshot(), TapActivity::new(2, 1, 960));
    }

    #[test]
    fn config_follows_the_platform_capability() {
        let mut caps = PlatformCaps::default();
        assert!(!WatchdogConfig::for_caps(&caps).emits_silence_when_idle);
        caps.emits_silence_when_idle = true;
        assert!(WatchdogConfig::for_caps(&caps).emits_silence_when_idle);
    }

    #[test]
    fn faults_render_a_length_a_human_can_read() {
        let f = Fault::NoBuffers {
            since_ns: 0,
            for_ns: 5_500_000_000,
        };
        assert_eq!(f.to_string(), "the tap delivered no audio for 5.5 s");
        assert_eq!(f.as_str(), "no-buffers");
        assert_eq!(f.since_ns(), 0);

        let f = Fault::SilentWhileOutputRunning {
            silent_since_ns: 1_000,
            corroborated_for_ns: 8_000_000_000,
        };
        assert!(f.to_string().contains("8.0 s of digital silence"));
        assert_eq!(f.since_ns(), 1_000);
    }
}
