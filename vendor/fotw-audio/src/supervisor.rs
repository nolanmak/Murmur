//! Recovery: tearing a dead tap down and building a new one mid-session.
//!
//! [`crate::watchdog`] decides *that* a tap is broken and
//! [`crate::device_change`] reports that the hardware moved. This module is
//! what does something about it, and it is where the two issues meet: the
//! response to both is the same full teardown and rebuild.
//!
//! # The rebuild is all-or-nothing, in order
//!
//! docs/REQUIREMENTS.md 6.4 and issue #25 both specify the whole sequence and
//! both say partial recovery — IOProc-only, aggregate-only — is unreliable:
//!
//! ```text
//! AudioDeviceStop -> AudioDeviceDestroyIOProcID
//!   -> AudioHardwareDestroyAggregateDevice -> AudioHardwareDestroyProcessTap
//!   -> recreate tap -> recreate aggregate -> new IOProc -> AudioDeviceStart
//! ```
//!
//! Above the seam that is: stop the tap, **drop it**, and only then ask the
//! factory for a new one. On macOS the drop is what performs the second half
//! of the teardown — `SystemTap`'s live objects are RAII guards whose
//! declaration order *is* destruction order — so a supervisor that reused the
//! old object would silently be doing the partial recovery the spec warns
//! against. `tests/watchdog.rs` asserts the ordering against a journalling
//! fake, because it is invisible in the type system.
//!
//! # Nothing already written is ever lost
//!
//! Recovery never touches the WAL, the ring, the encoder or the STT stream.
//! It replaces one `AudioTap` behind an unchanged [`crate::FrameSink`], so the
//! bytes already on disk are not rewound, truncated or reopened, and the
//! session continues into the same files.
//!
//! # A gap is a gap, and there are two kinds
//!
//! This is the part that is easy to get subtly and permanently wrong.
//!
//! When a tap **starves**, nothing at all was written for the outage. The
//! recording has a *hole*. If the samples either side are simply concatenated,
//! every timestamp after the hole moves earlier by the length of the outage —
//! note anchors, STT offsets, the two-stream alignment, the lot — and nothing
//! downstream can detect it. So [`GapKind::Unwritten`] carries
//! [`CaptureGap::frames_to_pad`]: the writer inserts exactly that much silence
//! and byte offset stays equal to session time.
//!
//! When a tap goes **digitally silent**, the IOProc kept firing and zeros were
//! written for every one of those seconds. The timeline is intact; only the
//! content is gone. Padding *that* would shift everything after the stall
//! later by the length of the stall — the same corruption in the opposite
//! direction. So [`GapKind::Silent`] pads nothing and exists purely to tell
//! the user, and support, that those minutes are not real audio.
//!
//! A silence stall therefore produces **two** gaps: the silent stretch that
//! was captured, then the rebuild window that was not.
//!
//! # Why recovery gives up, twice, for two different reasons
//!
//! **Rebuilds that fail.** A machine whose audio stack is genuinely broken
//! fails every rebuild, and retrying in a tight loop helps nobody. Attempts
//! are bounded ([`SupervisorConfig::max_attempts`]) and exponentially spaced,
//! and after the ceiling the supervisor says so once and stops. A device
//! change afterwards is new evidence — the user plugged something back in —
//! and re-arms it.
//!
//! **Rebuilds that succeed and change nothing.** This is the subtler one and
//! it is the safety net under the silence rule. A denied system-audio grant
//! delivers permanent silence and never an error (docs/REQUIREMENTS.md 6.3);
//! so does a genuinely quiet meeting, which the corroboration signal cannot
//! reliably tell apart (see [`crate::watchdog`]). Either way the rebuild
//! succeeds, the silence continues, and the fault recurs on a timer. Without a
//! second ceiling that is a rebuild every thirty seconds for the length of the
//! meeting, each one spending a gap and firing a notification — turning a bad
//! recording into a shredded one and training the user to ignore the warning
//! that matters.
//!
//! So a rebuild after which no audible sample ever arrives is counted as
//! *ineffective*, and after
//! [`SupervisorConfig::max_ineffective_recoveries`] of them the supervisor
//! stops acting on faults and says so once. It keeps watching: the moment a
//! single audible buffer arrives the count resets and normal supervision
//! resumes, so a meeting that was merely quiet costs a handful of gaps rather
//! than ninety of them.

use std::sync::Arc;
use std::time::Duration;

use crate::clock::Clock;
use crate::device_change::{Debounce, DeviceChangeKind, DeviceChangeSignal, DeviceChanges};
use crate::error::TapError;
use crate::format::StreamFormat;
use crate::frames::FrameSink;
use crate::ids::TapId;
use crate::tap::AudioTap;
use crate::watchdog::{Fault, OutputProbe, TapActivity, Verdict, Watchdog, WatchdogConfig};

/// Whether the writer has to insert anything for this gap.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GapKind {
    /// **No samples were captured for this interval.** The writer must insert
    /// [`CaptureGap::frames_to_pad`] frames of silence, or every timestamp
    /// after the gap shifts earlier by its length.
    Unwritten,
    /// **Samples were captured and they were digitally silent.** They are
    /// already in the file; inserting more would shift everything after the
    /// gap later. The marker exists to tell the user the content is lost.
    Silent,
}

/// Why a gap exists. Written into the session manifest verbatim.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GapReason {
    /// The tap stopped delivering buffers.
    TapStalledNoBuffers,
    /// The tap delivered bit-exact silence while output was running.
    TapStalledSilent,
    /// A device change forced a rebuild.
    DeviceChanged,
    /// The time spent tearing the old tap down and building the new one.
    RebuildWindow,
}

impl GapReason {
    /// A stable identifier for the manifest.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::TapStalledNoBuffers => "tap-stall-no-buffers",
            Self::TapStalledSilent => "tap-stall-silent",
            Self::DeviceChanged => "device-changed",
            Self::RebuildWindow => "rebuild-window",
        }
    }
}

impl std::fmt::Display for GapReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// An interval of the recording that is not what it appears to be.
///
/// Times are on the process-wide host clock ([`crate::clock`]), which is what
/// every tap stamps from, so the caller subtracts its own session epoch to get
/// the session-relative milliseconds the manifest wants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CaptureGap {
    /// Host-clock start.
    pub start_ns: u64,
    /// Host-clock end.
    pub end_ns: u64,
    /// Whether the writer must pad.
    pub kind: GapKind,
    /// Why it happened.
    pub reason: GapReason,
}

impl CaptureGap {
    /// Length in nanoseconds.
    #[must_use]
    pub const fn duration_ns(&self) -> u64 {
        self.end_ns.saturating_sub(self.start_ns)
    }

    /// Length in milliseconds, rounded down.
    #[must_use]
    pub const fn duration_ms(&self) -> u64 {
        self.duration_ns() / 1_000_000
    }

    /// Frames of silence the writer must insert to keep the timeline honest.
    ///
    /// Zero for [`GapKind::Silent`], whose samples are already in the file.
    #[must_use]
    pub fn frames_to_pad(&self, sample_rate_hz: u32) -> u64 {
        if self.kind == GapKind::Silent {
            return 0;
        }
        let ns = u128::from(self.duration_ns());
        let frames = ns * u128::from(sample_rate_hz) / 1_000_000_000;
        u64::try_from(frames).unwrap_or(u64::MAX)
    }

    /// Interleaved samples the writer must insert, for a given stream format.
    #[must_use]
    pub fn samples_to_pad(&self, format: StreamFormat) -> u64 {
        self.frames_to_pad(format.sample_rate_hz)
            .saturating_mul(u64::from(format.channels))
    }
}

/// Something the layer above needs to know, and usually the user too.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HealthEvent {
    /// A stall was detected and recovery is starting.
    ///
    /// Not shown to the user on its own — the recovery may be invisible and
    /// instant — but issue #25 asks for it in the session log so support can
    /// diagnose the macOS 26 defect from a bug report.
    StallDetected {
        /// Which tap.
        tap: TapId,
        /// What was wrong.
        fault: Fault,
    },
    /// A device change arrived and was acted on.
    DeviceChanged {
        /// Which tap.
        tap: TapId,
        /// What changed.
        changes: DeviceChanges,
        /// Whether it caused a full rebuild.
        rebuilt: bool,
    },
    /// The tap was rebuilt and capture is running again.
    Recovered {
        /// Which tap.
        tap: TapId,
        /// Which attempt succeeded, counting from 1.
        attempt: u32,
        /// The **new** authoritative format. A Bluetooth headset engaging HFP
        /// changes it mid-meeting, and a converter left on the old ASBD
        /// produces garbage audio rather than none.
        format: StreamFormat,
        /// What the recording lost, and what the writer must do about it.
        gaps: Vec<CaptureGap>,
    },
    /// A rebuild attempt failed; another is scheduled.
    RecoveryFailed {
        /// Which tap.
        tap: TapId,
        /// Which attempt failed, counting from 1.
        attempt: u32,
        /// The platform's complaint.
        error: String,
        /// How long until the next attempt.
        retry_in: Duration,
    },
    /// Recovery was abandoned after the attempt ceiling.
    GaveUp {
        /// Which tap.
        tap: TapId,
        /// How many attempts were made.
        attempts: u32,
        /// The last failure.
        error: String,
    },
    /// Rebuilding keeps succeeding and keeps not helping, so supervision has
    /// paused until audible audio returns.
    ///
    /// Distinct from [`HealthEvent::GaveUp`] because the cause is different
    /// and so is the advice: nothing failed, and the likeliest explanations
    /// are a denied system-audio grant or a meeting that is genuinely quiet.
    RecoveryIneffective {
        /// Which tap.
        tap: TapId,
        /// How many rebuilds produced no audible audio.
        rebuilds: u32,
        /// The fault that kept recurring.
        fault: Fault,
    },
    /// Silence that could not be corroborated has run past the notice
    /// threshold. §6.3's banner. Never a rebuild.
    NoAudioDetected {
        /// Which tap.
        tap: TapId,
        /// How long the silence has run.
        for_ns: u64,
    },
}

impl HealthEvent {
    /// Whether the user should be shown this.
    ///
    /// "A silent auto-recovery that loses 30 seconds is better than losing
    /// everything, but the user still needs to know the recording had a gap."
    #[must_use]
    pub const fn is_user_visible(&self) -> bool {
        match self {
            Self::Recovered { .. }
            | Self::GaveUp { .. }
            | Self::RecoveryIneffective { .. }
            | Self::NoAudioDetected { .. } => true,
            // Diagnostics. A stall that was recovered in 300 ms is a log line,
            // not an interruption; the Recovered event is what the user sees.
            Self::StallDetected { .. }
            | Self::DeviceChanged { .. }
            | Self::RecoveryFailed { .. } => false,
        }
    }

    /// What to tell the user.
    ///
    /// Always names the amount of audio at stake: "we recovered" without "and
    /// this much is missing" is the kind of reassurance that turns into a
    /// support ticket three weeks later when the transcript has a hole in it.
    #[must_use]
    pub fn user_message(&self) -> String {
        match self {
            Self::StallDetected { fault, .. } => format!("Audio capture stalled: {fault}."),
            Self::DeviceChanged {
                changes, rebuilt, ..
            } => {
                if *rebuilt {
                    format!("Audio device changed ({changes}); rebuilding capture.")
                } else {
                    format!("Audio device changed ({changes}); capture continues.")
                }
            }
            Self::Recovered { gaps, .. } => {
                let lost: u64 = gaps.iter().map(CaptureGap::duration_ns).sum();
                format!(
                    "Audio capture recovered. {} of this recording is missing.",
                    human_secs(lost)
                )
            }
            Self::RecoveryFailed { attempt, error, .. } => {
                format!("Audio capture rebuild attempt {attempt} failed: {error}")
            }
            Self::GaveUp {
                attempts, error, ..
            } => format!(
                "Audio capture could not be restarted after {attempts} attempts \
                 ({error}). Recording continues, but this track will be silent."
            ),
            Self::RecoveryIneffective { rebuilds, .. } => format!(
                "Restarting audio capture {rebuilds} times did not bring any audio \
                 back, so it will not be restarted again until audio returns. \
                 Either nothing is playing, or system audio recording is not \
                 allowed in System Settings > Privacy & Security > Screen & \
                 System Audio Recording."
            ),
            Self::NoAudioDetected { for_ns, .. } => format!(
                "No audio detected for {}. Check that system audio recording is \
                 allowed in System Settings > Privacy & Security > Screen & \
                 System Audio Recording.",
                human_secs(*for_ns)
            ),
        }
    }
}

fn human_secs(ns: u64) -> String {
    format!("{:.1} s", ns as f64 / 1e9)
}

/// What one [`CaptureSupervisor::poll`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PollOutcome {
    /// Not started, or stopped by the caller. Nothing is supervised.
    Stopped,
    /// Audio is flowing.
    Healthy,
    /// Silence that cannot be corroborated. Surface it; do not rebuild.
    NoAudio {
        /// How long the silence has run.
        for_ns: u64,
    },
    /// Waiting out a debounce window or a retry backoff.
    Waiting,
    /// The tap was torn down and rebuilt this poll.
    Rebuilt,
    /// A rebuild attempt failed; another is scheduled.
    Retrying,
    /// Recovery was abandoned. The session continues without this leg.
    Abandoned,
}

/// Thresholds and policy for [`CaptureSupervisor`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SupervisorConfig {
    /// Which tap this supervises, for events and logs.
    pub id: TapId,
    /// Stall-detection thresholds.
    pub watchdog: WatchdogConfig,
    /// How long the device-change notifications must be quiet before acting.
    ///
    /// Issue #26 specifies 300 ms: one AirPods connect raises several
    /// notifications, and rebuilding on the first means rebuilding again on
    /// the third.
    pub device_change_debounce: Duration,
    /// The longest the debounce may postpone a rebuild.
    ///
    /// A device that chatters would otherwise defer the rebuild forever, and
    /// "forever" is a meeting that never gets recorded.
    pub device_change_debounce_ceiling: Duration,
    /// Delay before the second rebuild attempt. Doubles each failure.
    pub retry_backoff: Duration,
    /// Ceiling on the doubling.
    pub max_retry_backoff: Duration,
    /// How many consecutive failed rebuilds before giving up.
    pub max_attempts: u32,
    /// How many rebuilds may succeed without bringing any audible audio back
    /// before supervision pauses.
    ///
    /// The safety net under the silence rule, whose corroboration signal
    /// cannot reliably distinguish a stalled tap from a quiet meeting (see
    /// [`crate::watchdog`]). Three, with the default 30 s threshold, means a
    /// meeting that simply goes quiet costs at most four short gaps and one
    /// notification over two minutes rather than one every thirty seconds
    /// forever — and the count resets the instant real audio arrives.
    pub max_ineffective_recoveries: u32,
    /// Which kinds of device change are worth a rebuild.
    ///
    /// Not everything the platform reports is. [`DeviceChangeKind::DeviceList`]
    /// is excluded by default because **our own rebuild changes the device
    /// list** — recreating the aggregate does — so acting on it is a
    /// self-sustaining loop. That was measured, not theorised: it turned a
    /// 45-second recording into 27 rebuilds on the development machine. See
    /// that variant's documentation.
    pub rebuild_triggers: DeviceChanges,
    /// The shortest interval between two device-change-driven rebuilds.
    ///
    /// The structural backstop under the same failure. A rebuild can provoke
    /// the very notification that would trigger the next one, and no filter on
    /// *which* notifications matter is guaranteed to catch every such path on
    /// every macOS version. A floor on the rate is the thing that cannot be
    /// out-argued. Stall faults are not subject to it — they have their own
    /// timers and their own ceiling — so a genuinely dead tap still recovers
    /// promptly.
    pub min_rebuild_interval: Duration,
    /// Whether a device change forces a full rebuild.
    ///
    /// **This is a genuine open question and the default is a deliberate
    /// choice, not an assumption.** Issue #26 says the aggregate is
    /// invalidated on a default-output change and the tap must be destroyed
    /// too. docs/REQUIREMENTS.md 6.0 says the opposite is now true: a
    /// *tap-only* aggregate — which is what this codebase builds — "removes
    /// drift compensation, the default-output-device lookup, and
    /// default-output-device-change tracking from the critical path entirely".
    /// Both cannot be right, and only real hardware can settle it.
    ///
    /// Default `true`, because the costs are wildly asymmetric: an unnecessary
    /// rebuild costs one ~300 ms gap, and a missed one costs the rest of the
    /// meeting. Set `false` to keep the audio and let the stall watchdog be
    /// the backstop — it is re-armed at the moment of the change, so a tap
    /// that does die takes `no_buffer_timeout` to notice instead of 300 ms.
    pub rebuild_on_device_change: bool,
}

impl Default for SupervisorConfig {
    fn default() -> Self {
        Self {
            id: TapId::system_default(),
            watchdog: WatchdogConfig::default(),
            device_change_debounce: Duration::from_millis(300),
            device_change_debounce_ceiling: Duration::from_secs(1),
            retry_backoff: Duration::from_millis(250),
            max_retry_backoff: Duration::from_secs(5),
            max_attempts: 5,
            max_ineffective_recoveries: 3,
            rebuild_triggers: DeviceChanges::of([
                DeviceChangeKind::DefaultOutput,
                DeviceChangeKind::DefaultInput,
                DeviceChangeKind::StreamFormat,
                DeviceChangeKind::DidWake,
            ]),
            min_rebuild_interval: Duration::from_secs(3),
            rebuild_on_device_change: true,
        }
    }
}

/// Opens a fresh tap. Called once per rebuild.
type OpenFn = Box<dyn FnMut() -> Result<Box<dyn AudioTap>, TapError> + Send>;

/// Produces the sink for a newly started tap.
///
/// It must deliver into the **same** downstream ring as the sink before it —
/// that is what makes the rebuild invisible to the WAL — so implementations
/// typically hand back a producer that was recycled when the previous sink was
/// dropped, rather than allocating a new one.
type SinkFn = Box<dyn FnMut() -> Box<dyn FrameSink> + Send>;

/// An outage in progress: what it will cost and how hard we have tried.
#[derive(Debug, Clone)]
struct Recovery {
    /// Start of the interval for which nothing is being written.
    unwritten_start_ns: u64,
    /// Silence that *was* written, if the trigger was a silence stall.
    silent_gap: Option<CaptureGap>,
    /// Reason to stamp on the unwritten gap.
    reason: GapReason,
    /// Attempts made so far.
    attempt: u32,
    /// Host time of the next attempt.
    next_attempt_ns: u64,
    /// The last failure, for the give-up message.
    last_error: String,
}

#[derive(Debug)]
enum State {
    /// Never started, or stopped.
    Idle,
    /// A tap is live and being watched.
    Running,
    /// The tap is gone and we are trying to build another.
    Recovering(Recovery),
    /// We stopped trying. The outage is kept so a device change can resume it
    /// without losing the gap that has been accumulating.
    Abandoned(Recovery),
    /// A tap **is** running and being watched, but rebuilding it has stopped
    /// helping, so faults are noted and not acted on. Released the moment an
    /// audible buffer arrives.
    Quarantined,
}

/// Keeps one tap alive for the length of a session.
///
/// Owns the tap, the stall watchdog and the device-change mailbox, and is
/// driven by a caller that polls it on whatever cadence suits — every 50 to
/// 250 ms in practice. It never sleeps, never spawns and never blocks, so it
/// composes with any runtime.
pub struct CaptureSupervisor {
    config: SupervisorConfig,
    clock: Arc<dyn Clock>,
    open: OpenFn,
    make_sink: SinkFn,
    tap: Option<Box<dyn AudioTap>>,
    format: Option<StreamFormat>,
    watchdog: Watchdog,
    signal: Arc<DeviceChangeSignal>,
    debounce: Debounce,
    pending_changes: DeviceChanges,
    state: State,
    events: Vec<HealthEvent>,
    rebuilds: u32,
    last_rebuild_ns: Option<u64>,
    ineffective_recoveries: u32,
    no_audio_notified: bool,
}

impl std::fmt::Debug for CaptureSupervisor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CaptureSupervisor")
            .field("id", &self.config.id)
            .field("state", &self.state)
            .field("format", &self.format)
            .field("rebuilds", &self.rebuilds)
            .field("pending_changes", &self.pending_changes)
            .field("queued_events", &self.events.len())
            .finish()
    }
}

impl CaptureSupervisor {
    /// Build a supervisor around a way of opening taps and a way of making
    /// sinks for them.
    pub fn new(
        config: SupervisorConfig,
        clock: Arc<dyn Clock>,
        open: impl FnMut() -> Result<Box<dyn AudioTap>, TapError> + Send + 'static,
        make_sink: impl FnMut() -> Box<dyn FrameSink> + Send + 'static,
    ) -> Self {
        let watchdog = Watchdog::new(config.watchdog);
        let debounce = Debounce::new(
            config.device_change_debounce,
            config.device_change_debounce_ceiling,
        );
        Self {
            config,
            clock,
            open: Box::new(open),
            make_sink: Box::new(make_sink),
            tap: None,
            format: None,
            watchdog,
            signal: DeviceChangeSignal::new(),
            debounce,
            pending_changes: DeviceChanges::empty(),
            state: State::Idle,
            events: Vec::new(),
            rebuilds: 0,
            last_rebuild_ns: None,
            ineffective_recoveries: 0,
            no_audio_notified: false,
        }
    }

    /// The mailbox a platform listener raises into.
    ///
    /// Handed to `MacOsPlatform::watch_devices`, or to whatever else can see
    /// the hardware move.
    #[must_use]
    pub fn signal(&self) -> Arc<DeviceChangeSignal> {
        Arc::clone(&self.signal)
    }

    /// Open and start the tap.
    pub fn start(&mut self) -> Result<StreamFormat, TapError> {
        if !matches!(self.state, State::Idle) {
            return Err(TapError::AlreadyRunning);
        }
        let mut tap = (self.open)()?;
        let format = tap.start((self.make_sink)())?;
        let now = self.clock.now_ns();
        self.tap = Some(tap);
        self.format = Some(format);
        self.watchdog.arm(now);
        self.state = State::Running;
        Ok(format)
    }

    /// Stop supervising and stop the tap.
    ///
    /// Idempotent. A stopped supervisor never rebuilds: the commonest reason a
    /// tap stops delivering is that the user ended the meeting, and treating
    /// that as a fault would resurrect capture after the recording closed.
    pub fn stop(&mut self) -> Result<(), TapError> {
        let result = match self.tap.take() {
            Some(mut tap) => tap.stop(),
            None => Ok(()),
        };
        self.state = State::Idle;
        self.debounce.clear();
        result
    }

    /// The current authoritative format, if a tap is running.
    #[must_use]
    pub const fn format(&self) -> Option<StreamFormat> {
        self.format
    }

    /// How many times the tap has been rebuilt.
    #[must_use]
    pub const fn rebuilds(&self) -> u32 {
        self.rebuilds
    }

    /// Whether supervision is paused because rebuilding stopped helping.
    ///
    /// A tap **is** still running and its audio is still being written; what
    /// has stopped is acting on its faults. Worth surfacing — a session
    /// running in this state is one where the recording is probably silent,
    /// and it is released the moment an audible buffer arrives.
    #[must_use]
    pub const fn is_quarantined(&self) -> bool {
        matches!(self.state, State::Quarantined)
    }

    /// Take everything that has happened since the last drain.
    ///
    /// Drained rather than replayed so the caller cannot notify the user twice
    /// for one gap.
    pub fn drain_events(&mut self) -> Vec<HealthEvent> {
        std::mem::take(&mut self.events)
    }

    /// Observe the tap once and act.
    ///
    /// `activity` is a snapshot of [`crate::watchdog::ActivityCounters`] the
    /// sink has been bumping; `probe` answers whether anything on the machine
    /// is rendering output, and is consulted only when it is needed.
    pub fn poll(&mut self, activity: TapActivity, probe: &impl OutputProbe) -> PollOutcome {
        let now = self.clock.now_ns();

        let changes = self.signal.take();
        if !changes.is_empty() {
            for kind in changes.kinds() {
                self.pending_changes = self.pending_changes.with(kind);
            }
            // Only the kinds worth acting on arm the debounce. The rest are
            // still recorded so the event names everything that happened, but
            // a device-list change on its own must never start a rebuild —
            // our own rebuild causes one.
            if changes.intersects(self.config.rebuild_triggers) {
                self.debounce.signal(now);
            }
        }

        match std::mem::replace(&mut self.state, State::Idle) {
            State::Idle => {
                self.state = State::Idle;
                PollOutcome::Stopped
            }
            State::Recovering(recovery) => {
                if now >= recovery.next_attempt_ns {
                    self.attempt_rebuild(now, recovery)
                } else {
                    self.state = State::Recovering(recovery);
                    PollOutcome::Waiting
                }
            }
            State::Abandoned(recovery) => {
                // A device change after giving up is new evidence — the user
                // plugged something back in — and is worth another go.
                if self.debounce.poll(now) {
                    let resumed = Recovery {
                        attempt: 0,
                        next_attempt_ns: now,
                        ..recovery
                    };
                    self.attempt_rebuild(now, resumed)
                } else {
                    self.state = State::Abandoned(recovery);
                    PollOutcome::Abandoned
                }
            }
            State::Running => {
                self.state = State::Running;
                self.poll_watching(now, activity, probe, false)
            }
            State::Quarantined => {
                self.state = State::Quarantined;
                self.poll_watching(now, activity, probe, true)
            }
        }
    }

    fn poll_watching(
        &mut self,
        now: u64,
        activity: TapActivity,
        probe: &impl OutputProbe,
        quarantined: bool,
    ) -> PollOutcome {
        let verdict = self.watchdog.poll(now, activity, probe);

        // Audible audio is the only thing that proves a rebuild worked, and it
        // is what releases a quarantine.
        if self.watchdog.audible_since_arm() {
            self.ineffective_recoveries = 0;
            if quarantined {
                self.state = State::Running;
            }
        }

        // A fault outranks a pending device change: both lead to the same
        // rebuild, and the fault carries the better gap boundaries.
        if let Verdict::Faulted(fault) = verdict {
            if quarantined && !self.watchdog.audible_since_arm() {
                // Already told the user. Rebuilding again would spend another
                // gap to reach the same place.
                return PollOutcome::Abandoned;
            }
            self.events.push(HealthEvent::StallDetected {
                tap: self.config.id.clone(),
                fault,
            });
            if self.watchdog.audible_since_arm() {
                self.ineffective_recoveries = 0;
            } else {
                self.ineffective_recoveries += 1;
                if self.ineffective_recoveries > self.config.max_ineffective_recoveries {
                    self.events.push(HealthEvent::RecoveryIneffective {
                        tap: self.config.id.clone(),
                        rebuilds: self.ineffective_recoveries - 1,
                        fault,
                    });
                    // Re-arm so the next fault is measured from here rather
                    // than firing again on the next poll.
                    self.watchdog.arm(now);
                    self.state = State::Quarantined;
                    return PollOutcome::Abandoned;
                }
            }
            let (silent_gap, unwritten_start_ns, reason) = match fault {
                Fault::NoBuffers { since_ns, .. } => {
                    (None, since_ns, GapReason::TapStalledNoBuffers)
                }
                Fault::SilentWhileOutputRunning {
                    silent_since_ns, ..
                } => (
                    Some(CaptureGap {
                        start_ns: silent_since_ns,
                        end_ns: now,
                        kind: GapKind::Silent,
                        reason: GapReason::TapStalledSilent,
                    }),
                    now,
                    GapReason::RebuildWindow,
                ),
            };
            return self.begin_recovery(now, silent_gap, unwritten_start_ns, reason);
        }

        if self.debounce.poll(now) {
            let changes = std::mem::replace(&mut self.pending_changes, DeviceChanges::empty());
            let too_soon = self.last_rebuild_ns.is_some_and(|last| {
                now.saturating_sub(last) < self.config.min_rebuild_interval.as_nanos() as u64
            });
            if too_soon {
                // A rebuild can provoke the notification that would trigger
                // the next one. Refusing here is what makes that terminate,
                // and it costs nothing real: the tap is alive, and the stall
                // watchdog — re-armed from this moment — is still the backstop
                // if the change did kill it.
                self.events.push(HealthEvent::DeviceChanged {
                    tap: self.config.id.clone(),
                    changes,
                    rebuilt: false,
                });
                self.watchdog.arm(now);
                return PollOutcome::Healthy;
            }
            if self.config.rebuild_on_device_change {
                self.events.push(HealthEvent::DeviceChanged {
                    tap: self.config.id.clone(),
                    changes,
                    rebuilt: true,
                });
                return self.begin_recovery(now, None, now, GapReason::DeviceChanged);
            }
            // Policy says the tap survives a device change. Re-arm anyway: the
            // stall clock must now measure from the change, not from the last
            // buffer before it, or a tap that died in the switch is given
            // credit for audio that arrived before the hardware moved.
            self.events.push(HealthEvent::DeviceChanged {
                tap: self.config.id.clone(),
                changes,
                rebuilt: false,
            });
            self.watchdog.arm(now);
            return PollOutcome::Healthy;
        }

        if self.debounce.is_pending() {
            return PollOutcome::Waiting;
        }

        match verdict {
            Verdict::NoAudio { for_ns } => {
                if !self.no_audio_notified {
                    self.no_audio_notified = true;
                    self.events.push(HealthEvent::NoAudioDetected {
                        tap: self.config.id.clone(),
                        for_ns,
                    });
                }
                PollOutcome::NoAudio { for_ns }
            }
            Verdict::Healthy => {
                self.no_audio_notified = false;
                PollOutcome::Healthy
            }
            Verdict::Faulted(_) => unreachable!("faults return above"),
        }
    }

    /// Tear the tap down and start trying to build another.
    fn begin_recovery(
        &mut self,
        now: u64,
        silent_gap: Option<CaptureGap>,
        unwritten_start_ns: u64,
        reason: GapReason,
    ) -> PollOutcome {
        // The full sequence, in order: stop, then destroy. Dropping the tap is
        // what destroys the aggregate and then the tap itself, and it happens
        // before anything new is created.
        if let Some(mut tap) = self.tap.take() {
            // A stop that fails still has to be followed by the drop: the
            // whole point of the rebuild is that the old objects go away.
            let _ = tap.stop();
        }
        self.format = None;
        let recovery = Recovery {
            unwritten_start_ns,
            silent_gap,
            reason,
            attempt: 0,
            next_attempt_ns: now,
            last_error: String::new(),
        };
        self.attempt_rebuild(now, recovery)
    }

    fn attempt_rebuild(&mut self, now: u64, mut recovery: Recovery) -> PollOutcome {
        recovery.attempt += 1;

        let started = (self.open)().and_then(|mut tap| {
            let format = tap.start((self.make_sink)())?;
            Ok((tap, format))
        });

        let (tap, format) = match started {
            Ok(pair) => pair,
            Err(e) => {
                recovery.last_error = e.to_string();
                if recovery.attempt >= self.config.max_attempts {
                    self.events.push(HealthEvent::GaveUp {
                        tap: self.config.id.clone(),
                        attempts: recovery.attempt,
                        error: recovery.last_error.clone(),
                    });
                    self.state = State::Abandoned(recovery);
                    return PollOutcome::Abandoned;
                }
                let retry_in = self.backoff(recovery.attempt);
                self.events.push(HealthEvent::RecoveryFailed {
                    tap: self.config.id.clone(),
                    attempt: recovery.attempt,
                    error: recovery.last_error.clone(),
                    retry_in,
                });
                recovery.next_attempt_ns = now.saturating_add(retry_in.as_nanos() as u64);
                self.state = State::Recovering(recovery);
                return PollOutcome::Retrying;
            }
        };

        let mut gaps = Vec::with_capacity(2);
        if let Some(g) = recovery.silent_gap {
            gaps.push(g);
        }
        gaps.push(CaptureGap {
            start_ns: recovery.unwritten_start_ns,
            end_ns: now,
            kind: GapKind::Unwritten,
            reason: recovery.reason,
        });

        self.tap = Some(tap);
        self.format = Some(format);
        self.rebuilds += 1;
        self.last_rebuild_ns = Some(now);
        self.watchdog.arm(now);
        // A change that arrived during the outage has been answered by this
        // rebuild. Leaving it pending would fire a second, pointless rebuild
        // 300 ms later — and spend another gap on it.
        self.debounce.clear();
        self.pending_changes = DeviceChanges::empty();
        self.no_audio_notified = false;
        self.state = State::Running;
        self.events.push(HealthEvent::Recovered {
            tap: self.config.id.clone(),
            attempt: recovery.attempt,
            format,
            gaps,
        });
        PollOutcome::Rebuilt
    }

    /// Exponential, capped. The first retry is immediate-ish and the last ones
    /// are far enough apart not to hammer a HAL that is already unhappy.
    fn backoff(&self, attempt: u32) -> Duration {
        let shift = attempt.saturating_sub(1).min(16);
        let scaled = self
            .config
            .retry_backoff
            .saturating_mul(1u32 << shift.min(16));
        scaled.min(self.config.max_retry_backoff)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_silent_gap_is_never_padded_and_an_unwritten_one_always_is() {
        let silent = CaptureGap {
            start_ns: 0,
            end_ns: 8_000_000_000,
            kind: GapKind::Silent,
            reason: GapReason::TapStalledSilent,
        };
        assert_eq!(silent.duration_ms(), 8_000);
        assert_eq!(
            silent.frames_to_pad(48_000),
            0,
            "those samples are already in the file"
        );

        let hole = CaptureGap {
            kind: GapKind::Unwritten,
            reason: GapReason::TapStalledNoBuffers,
            ..silent
        };
        assert_eq!(hole.frames_to_pad(48_000), 384_000);
        assert_eq!(
            hole.samples_to_pad(StreamFormat::new(48_000, 2, crate::SampleFormat::F32)),
            768_000
        );
    }

    #[test]
    fn user_messages_name_the_cost() {
        let e = HealthEvent::Recovered {
            tap: TapId::system_default(),
            attempt: 1,
            format: StreamFormat::new(48_000, 2, crate::SampleFormat::F32),
            gaps: vec![CaptureGap {
                start_ns: 0,
                end_ns: 5_000_000_000,
                kind: GapKind::Unwritten,
                reason: GapReason::TapStalledNoBuffers,
            }],
        };
        assert!(e.is_user_visible());
        assert!(e.user_message().contains("5.0 s"), "{}", e.user_message());

        let quiet = HealthEvent::NoAudioDetected {
            tap: TapId::system_default(),
            for_ns: 30_000_000_000,
        };
        assert!(quiet.is_user_visible());
        assert!(quiet.user_message().contains("30.0 s"));
        // The commonest cause is a denied grant, so the message has to say
        // where to fix it rather than only that something is wrong.
        assert!(
            quiet
                .user_message()
                .contains("Screen & System Audio Recording")
        );
    }

    #[test]
    fn gap_reasons_have_stable_manifest_strings() {
        assert_eq!(
            GapReason::TapStalledNoBuffers.to_string(),
            "tap-stall-no-buffers"
        );
        assert_eq!(GapReason::TapStalledSilent.to_string(), "tap-stall-silent");
        assert_eq!(GapReason::DeviceChanged.to_string(), "device-changed");
        assert_eq!(GapReason::RebuildWindow.to_string(), "rebuild-window");
    }
}
