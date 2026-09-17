//! The one host clock every tap stamps from (seam rule 3).
//!
//! docs/REQUIREMENTS.md 6.5 specifies "ONE process-wide monotonic clock shared
//! by every tap: `mach_continuous_time` (macOS), QPC (Windows),
//! `CLOCK_MONOTONIC`/`pw_time` (Linux)". [`std::time::Instant`] is exactly
//! those primitives on each target, so the seam needs no platform code and no
//! dependency to satisfy the rule.
//!
//! **Known deviation, to be resolved when the macOS tap lands (CAP-09).**
//! `Instant` on Apple platforms reads `CLOCK_UPTIME_RAW` — the
//! `mach_absolute_time` family — which *pauses while the machine is asleep*,
//! whereas `mach_continuous_time` keeps counting. Both are monotonic, so
//! nothing here breaks, but the sleep/wake gap marker will understate the gap
//! by the sleep duration if it is derived from this clock alone. The macOS
//! backend should stamp from `mach_continuous_time` and, at that point, this
//! module grows a platform-specific body rather than every tap growing its own
//! clock.
//!
//! # Why there is a trait as well as a function
//!
//! The stall watchdog's thresholds are 5 and 8 seconds. Testing it against
//! [`host_ns`] would mean a test suite that sleeps, and a 13-second test is a
//! test nobody runs. [`Clock`] is the injection point: production passes
//! [`HostClock`], tests pass `testing::ManualClock`, and the detection logic
//! itself never reads a clock at all — it takes `now_ns` as an argument.

use std::sync::OnceLock;
use std::time::Instant;

/// A source of monotonic nanoseconds.
///
/// `Send + Sync` because the supervisor holds one behind an `Arc` and is
/// driven from a different thread than the one that built it.
pub trait Clock: Send + Sync {
    /// Nanoseconds on this clock. Must be monotonically non-decreasing.
    fn now_ns(&self) -> u64;
}

/// The real clock: the process-wide monotonic host clock of [`host_ns`].
#[derive(Debug, Clone, Copy, Default)]
pub struct HostClock;

impl Clock for HostClock {
    fn now_ns(&self) -> u64 {
        host_ns()
    }
}

/// The process-wide epoch. Fixed at the first call so every tap in the process
/// shares one zero point and cross-stream alignment is a subtraction.
static EPOCH: OnceLock<Instant> = OnceLock::new();

/// Nanoseconds on the process-wide monotonic host clock.
///
/// Monotonically non-decreasing for the life of the process. The absolute
/// value is meaningless on its own; only differences between two readings are.
#[must_use]
pub fn host_ns() -> u64 {
    let epoch = *EPOCH.get_or_init(Instant::now);
    ns_since(epoch)
}

/// Nanoseconds elapsed on the host clock since `origin`, saturating.
#[must_use]
pub fn ns_since(origin: Instant) -> u64 {
    u64::try_from(Instant::now().saturating_duration_since(origin).as_nanos()).unwrap_or(u64::MAX)
}

/// The process-wide epoch, initialising it if this is the first call.
///
/// Useful when a backend wants to convert its own device clock into the shared
/// timeline without re-reading [`host_ns`] per callback.
#[must_use]
pub fn epoch() -> Instant {
    *EPOCH.get_or_init(Instant::now)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_ns_is_monotonic() {
        let mut last = host_ns();
        for _ in 0..1_000 {
            let now = host_ns();
            assert!(now >= last, "{now} < {last}");
            last = now;
        }
    }

    #[test]
    fn the_epoch_is_stable_across_calls() {
        assert_eq!(epoch(), epoch());
    }
}
