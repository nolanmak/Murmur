//! CAP-06: the hardware moving underneath a live recording.
//!
//! AirPods connecting, an HDMI cable, a dock, a lid closing. This is the
//! commonest real-world tap failure and it always happens mid-meeting: the
//! default output changes, and audio silently stops.
//!
//! # The constraint that shapes this module
//!
//! macOS delivers these through `AudioObjectAddPropertyListenerBlock` on a
//! Core Audio-owned dispatch queue. That block **must not block and must not
//! allocate**: it is not the IOProc's real-time thread, but it is a thread the
//! HAL is waiting on, and a `malloc` there can wait on the allocator lock
//! behind any other thread in the process.
//!
//! Publishing onto [`crate::EventBus`] from there is therefore not an option —
//! it takes a mutex, clones an event containing a `String`, and pushes into an
//! `mpsc` channel that allocates a node per message. So the listener does the
//! smallest thing that cannot fail: two atomic read-modify-writes into
//! [`DeviceChangeSignal`]. Everything that decides what to *do* about the
//! change runs on the supervisor's own thread, which may allocate, log and
//! take as long as it likes.
//!
//! `tests/no_alloc_signal.rs` asserts the allocation-free half against a
//! counting global allocator, because "this does not allocate" is exactly the
//! kind of claim that rots.
//!
//! # Why the changes coalesce
//!
//! One AirPods connect raises several notifications — the device list changes,
//! then the default output changes, sometimes twice as the link negotiates.
//! Each one arrives as a bit in a mask, so a burst of forty notifications is
//! one set of pending changes and one rebuild rather than forty.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::time::Duration;

/// A kind of change the platform can report.
///
/// Deliberately the four macOS listeners issue #26 names, expressed without
/// naming macOS: `kAudioHardwarePropertyDefaultOutputDevice`,
/// `kAudioHardwarePropertyDefaultInputDevice`,
/// `kAudioHardwarePropertyDevices`, and any ASBD change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DeviceChangeKind {
    /// The system default output device changed.
    DefaultOutput,
    /// The system default input device changed — the mic leg's equivalent.
    DefaultInput,
    /// A device appeared or disappeared.
    ///
    /// **Deliberately not a rebuild trigger by default**, and this is not a
    /// preference — it is a measured feedback loop. Rebuilding the tap
    /// destroys and recreates an aggregate device, which changes
    /// `kAudioHardwarePropertyDevices`, which arrives here, which rebuilds the
    /// tap. Observed on a real Mac: a 45-second recording shredded into 27
    /// rebuilds. `kAudioAggregateDeviceIsPrivateKey` keeps the aggregate out
    /// of the user's Sound settings; it does *not* keep it out of the device
    /// list.
    ///
    /// Nothing is lost by ignoring it. A device appearing or disappearing only
    /// affects a live tap when it becomes, or stops being, the default — and
    /// that raises [`DeviceChangeKind::DefaultOutput`] or
    /// [`DeviceChangeKind::DefaultInput`] in its own right. The signal is
    /// still raised and still reported, because a device picker wants it.
    DeviceList,
    /// A stream format changed.
    ///
    /// A Bluetooth headset engaging HFP changes the rate on **both** legs at
    /// once (48 kHz → 16/24 kHz). docs/REQUIREMENTS.md and issue #26 both say
    /// to treat any ASBD change as a full rebuild trigger rather than a
    /// converter reconfiguration, because a converter left holding the old
    /// ASBD produces garbage audio instead of no audio — which is worse, since
    /// nothing downstream can tell.
    StreamFormat,
    /// The machine is about to sleep.
    ///
    /// **Nothing in this crate raises this yet.** Sleep and wake come from
    /// `NSWorkspace.willSleepNotification` / `didWakeNotification`, which is
    /// AppKit and therefore `fotw-shell`'s to observe; the variant exists so
    /// that when it does, it feeds the same debounce and the same rebuild as
    /// every other change rather than growing a parallel path. The
    /// idle-sleep assertion issue #26 also asks for (`IOPMAssertionCreate`)
    /// is likewise a shell concern.
    WillSleep,
    /// The machine woke.
    ///
    /// The HAL topology is not guaranteed valid across sleep, so this is a
    /// rebuild trigger in its own right and not merely a hint.
    ///
    /// Not raised here yet either — see [`DeviceChangeKind::WillSleep`] — and
    /// there is a second problem waiting when it is. The gap a wake produces
    /// is measured on [`crate::clock`], and on Apple platforms that clock
    /// *pauses while the machine is asleep* (`CLOCK_UPTIME_RAW`, the
    /// documented CAP-09 deviation). A five-minute lid-close would therefore
    /// be recorded as a gap of a few milliseconds and every timestamp after it
    /// would be five minutes early. Whoever wires the notification must also
    /// give the gap a wall-clock length.
    DidWake,
}

impl DeviceChangeKind {
    /// Every kind, for iteration.
    pub const ALL: [Self; 6] = [
        Self::DefaultOutput,
        Self::DefaultInput,
        Self::DeviceList,
        Self::StreamFormat,
        Self::WillSleep,
        Self::DidWake,
    ];

    const fn bit(self) -> u32 {
        match self {
            Self::DefaultOutput => 1,
            Self::DefaultInput => 2,
            Self::DeviceList => 4,
            Self::StreamFormat => 8,
            Self::WillSleep => 16,
            Self::DidWake => 32,
        }
    }

    /// A stable identifier for logs and gap reasons.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::DefaultOutput => "default-output",
            Self::DefaultInput => "default-input",
            Self::DeviceList => "device-list",
            Self::StreamFormat => "stream-format",
            Self::WillSleep => "will-sleep",
            Self::DidWake => "did-wake",
        }
    }
}

impl std::fmt::Display for DeviceChangeKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A set of pending changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DeviceChanges(u32);

impl DeviceChanges {
    /// The empty set.
    #[must_use]
    pub const fn empty() -> Self {
        Self(0)
    }

    /// Nothing pending.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// Whether `kind` is in the set.
    #[must_use]
    pub const fn contains(self, kind: DeviceChangeKind) -> bool {
        self.0 & kind.bit() != 0
    }

    /// Add a kind.
    #[must_use]
    pub const fn with(self, kind: DeviceChangeKind) -> Self {
        Self(self.0 | kind.bit())
    }

    /// A set built from an iterator of kinds.
    #[must_use]
    pub fn of(kinds: impl IntoIterator<Item = DeviceChangeKind>) -> Self {
        kinds.into_iter().fold(Self::empty(), Self::with)
    }

    /// Whether any kind is in both sets.
    #[must_use]
    pub const fn intersects(self, other: Self) -> bool {
        self.0 & other.0 != 0
    }

    /// The kinds in the set, in [`DeviceChangeKind::ALL`] order.
    #[must_use]
    pub fn kinds(self) -> Vec<DeviceChangeKind> {
        DeviceChangeKind::ALL
            .into_iter()
            .filter(|k| self.contains(*k))
            .collect()
    }
}

impl std::fmt::Display for DeviceChanges {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let names: Vec<&str> = self.kinds().iter().map(|k| k.as_str()).collect();
        f.write_str(&names.join("+"))
    }
}

/// The lock-free mailbox between a platform notification and the supervisor.
///
/// [`raise`](Self::raise) is wait-free and allocation-free and is the only
/// thing a Core Audio listener block does. [`take`](Self::take) is the
/// supervisor's side and clears what it returns, so one AirPods connect
/// produces one rebuild rather than a rebuild per poll forever.
#[derive(Debug, Default)]
pub struct DeviceChangeSignal {
    pending: AtomicU32,
    raises: AtomicU64,
}

impl DeviceChangeSignal {
    /// A signal with nothing pending, ready to share with a listener.
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Record that `kind` happened.
    ///
    /// Callable from a Core Audio notification thread: two relaxed
    /// read-modify-writes, no allocation, no lock, no syscall, no branch that
    /// can re-enter the runtime.
    pub fn raise(&self, kind: DeviceChangeKind) {
        // Release so a supervisor that observes the bit also observes anything
        // the listener wrote before raising it.
        self.pending.fetch_or(kind.bit(), Ordering::Release);
        self.raises.fetch_add(1, Ordering::Relaxed);
    }

    /// Take and clear everything pending.
    #[must_use]
    pub fn take(&self) -> DeviceChanges {
        DeviceChanges(self.pending.swap(0, Ordering::Acquire))
    }

    /// Look without clearing.
    #[must_use]
    pub fn peek(&self) -> DeviceChanges {
        DeviceChanges(self.pending.load(Ordering::Acquire))
    }

    /// How many notifications have been raised in total.
    ///
    /// Diagnostics only: a burst of forty coalesces into one set of pending
    /// changes, and this is how a bug report can still show it was forty.
    #[must_use]
    pub fn raises(&self) -> u64 {
        self.raises.load(Ordering::Relaxed)
    }
}

/// Trailing-edge debounce with a ceiling.
///
/// Issue #26 asks for 300 ms of quiet before rebuilding, because one physical
/// event produces a burst of notifications and rebuilding on the first one
/// means rebuilding again on the third.
///
/// The ceiling is not decoration. A pure trailing-edge debounce can be
/// postponed forever by a device that keeps chattering — a flaky dock, a
/// Bluetooth link renegotiating — and "forever" here means a meeting that is
/// never recorded because the rebuild was always one notification away.
#[derive(Debug, Clone)]
pub struct Debounce {
    quiet: Duration,
    ceiling: Duration,
    first_ns: Option<u64>,
    last_ns: u64,
}

impl Debounce {
    /// Fire once the signals have been quiet for `quiet`, or `ceiling` after
    /// the first one, whichever comes first.
    #[must_use]
    pub const fn new(quiet: Duration, ceiling: Duration) -> Self {
        Self {
            quiet,
            ceiling,
            first_ns: None,
            last_ns: 0,
        }
    }

    /// Note that something happened at `now_ns`.
    pub const fn signal(&mut self, now_ns: u64) {
        if self.first_ns.is_none() {
            self.first_ns = Some(now_ns);
        }
        self.last_ns = now_ns;
    }

    /// Whether a rebuild is waiting to happen.
    #[must_use]
    pub const fn is_pending(&self) -> bool {
        self.first_ns.is_some()
    }

    /// True exactly once per burst, when it is time to act. Resets itself.
    pub fn poll(&mut self, now_ns: u64) -> bool {
        let Some(first) = self.first_ns else {
            return false;
        };
        let quiet_enough = now_ns.saturating_sub(self.last_ns) >= self.quiet.as_nanos() as u64;
        let waited_enough = now_ns.saturating_sub(first) >= self.ceiling.as_nanos() as u64;
        if quiet_enough || waited_enough {
            self.first_ns = None;
            return true;
        }
        false
    }

    /// Forget a pending burst without acting on it.
    pub const fn clear(&mut self) {
        self.first_ns = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: u64 = 1_000_000;

    #[test]
    fn kinds_have_distinct_bits() {
        let mut seen = 0u32;
        for k in DeviceChangeKind::ALL {
            assert_eq!(seen & k.bit(), 0, "{k} collides with an earlier kind");
            seen |= k.bit();
        }
        assert_eq!(seen.count_ones(), DeviceChangeKind::ALL.len() as u32);
    }

    #[test]
    fn a_set_renders_its_members() {
        let set = DeviceChanges::empty()
            .with(DeviceChangeKind::DeviceList)
            .with(DeviceChangeKind::DefaultOutput);
        assert_eq!(set.to_string(), "default-output+device-list");
        assert_eq!(
            set.kinds(),
            vec![
                DeviceChangeKind::DefaultOutput,
                DeviceChangeKind::DeviceList
            ]
        );
    }

    #[test]
    fn peek_does_not_clear_but_take_does() {
        let s = DeviceChangeSignal::new();
        s.raise(DeviceChangeKind::DidWake);
        assert!(s.peek().contains(DeviceChangeKind::DidWake));
        assert!(s.peek().contains(DeviceChangeKind::DidWake));
        assert!(s.take().contains(DeviceChangeKind::DidWake));
        assert!(s.take().is_empty());
    }

    #[test]
    fn a_quiet_burst_fires_once_after_the_quiet_window() {
        let mut d = Debounce::new(Duration::from_millis(300), Duration::from_secs(1));
        assert!(!d.poll(0), "nothing pending");

        d.signal(0);
        d.signal(100 * MS);
        d.signal(200 * MS);
        assert!(!d.poll(400 * MS), "only 200 ms since the last signal");
        assert!(d.poll(500 * MS), "300 ms of quiet");
        assert!(!d.poll(10_000 * MS), "and it does not fire twice");
    }

    #[test]
    fn a_chattering_device_hits_the_ceiling() {
        let mut d = Debounce::new(Duration::from_millis(300), Duration::from_secs(1));
        let mut now = 0;
        d.signal(now);
        for _ in 0..12 {
            now += 100 * MS;
            d.signal(now);
            if d.poll(now) {
                assert_eq!(now, 1_000 * MS, "the ceiling, not the quiet window");
                return;
            }
        }
        panic!("the ceiling never fired");
    }
}
