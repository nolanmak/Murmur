//! Reconnect backoff and the attempt budget (spec 4.2, STT-09).
//!
//! The schedule the spec names — base 250 ms, ×2, ±20 % jitter, cap 8 s, at
//! most 6 attempts in a rolling 60 s window — is here as pure arithmetic with
//! the two nondeterministic inputs (the jitter draw and the current time) passed
//! in rather than read. That is what makes the schedule assertable to the
//! millisecond in a test instead of observable only as "roughly a second-ish".
//!
//! The rolling window matters more than the cap. Without it, a provider that
//! accepts a socket and drops it immediately produces an unbounded reconnect
//! loop that never trips the failover chain, because every individual attempt
//! "succeeded".

use std::collections::VecDeque;
use std::hash::{BuildHasher, Hasher};

/// First retry delay before jitter, in milliseconds.
pub const DEFAULT_BASE_MS: u64 = 250;
/// The delay ceiling before jitter, in milliseconds.
pub const DEFAULT_CAP_MS: u64 = 8_000;
/// Jitter as a fraction of the delay, applied symmetrically: ±20 %.
pub const DEFAULT_JITTER_FRACTION: f64 = 0.20;
/// Attempts permitted inside one window before the stream gives up.
pub const DEFAULT_MAX_ATTEMPTS: u32 = 6;
/// The width of the rolling attempt window, in milliseconds.
pub const DEFAULT_WINDOW_MS: u64 = 60_000;

/// A source of jitter in `[0.0, 1.0)`.
///
/// A trait rather than a closure so tests can pin the draw and assert exact
/// delays; production uses [`ProcessJitter`].
pub trait Jitter: Send {
    /// The next draw, which implementations must keep in `[0.0, 1.0)`.
    fn unit(&mut self) -> f64;
}

/// A jitter source that always returns the same draw. For tests.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FixedJitter(pub f64);

impl Jitter for FixedJitter {
    fn unit(&mut self) -> f64 {
        self.0.clamp(0.0, 1.0 - f64::EPSILON)
    }
}

/// The production jitter source: a xorshift64* seeded from the standard
/// library's per-process random state.
///
/// Deliberately not a `rand` dependency. Backoff jitter exists to decorrelate
/// two streams of ours that dropped at the same instant, which is a statistical
/// requirement of the very weakest kind — it does not need a CSPRNG, and adding
/// one to satisfy it would be a dependency bought with nothing.
#[derive(Debug, Clone)]
pub struct ProcessJitter {
    state: u64,
}

impl Default for ProcessJitter {
    fn default() -> Self {
        Self::new()
    }
}

impl ProcessJitter {
    /// A generator seeded from process entropy.
    #[must_use]
    pub fn new() -> Self {
        let seed = std::collections::hash_map::RandomState::new()
            .build_hasher()
            .finish();
        Self::from_seed(seed)
    }

    /// A generator with an explicit seed, for reproducible runs.
    #[must_use]
    pub fn from_seed(seed: u64) -> Self {
        Self {
            // A zero state is a fixed point of xorshift, so it must not survive.
            state: if seed == 0 {
                0x9E37_79B9_7F4A_7C15
            } else {
                seed
            },
        }
    }
}

impl Jitter for ProcessJitter {
    fn unit(&mut self) -> f64 {
        self.state ^= self.state >> 12;
        self.state ^= self.state << 25;
        self.state ^= self.state >> 27;
        let value = self.state.wrapping_mul(0x2545_F491_4F6C_DD1D);
        // 53 bits is exactly the f64 mantissa, so this is uniform and never 1.0.
        ((value >> 11) as f64) / ((1u64 << 53) as f64)
    }
}

/// The reconnect schedule (STT-09).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BackoffPolicy {
    /// Delay for attempt 0, before jitter.
    pub base_ms: u64,
    /// Multiplier applied per attempt.
    pub factor: u32,
    /// Delay ceiling, before jitter.
    pub cap_ms: u64,
    /// Symmetric jitter as a fraction of the delay.
    pub jitter_fraction: f64,
    /// Attempts allowed inside `window_ms`.
    pub max_attempts: u32,
    /// Width of the rolling attempt window.
    pub window_ms: u64,
}

impl Default for BackoffPolicy {
    fn default() -> Self {
        Self::spec()
    }
}

impl BackoffPolicy {
    /// Exactly the schedule STT-09 specifies.
    #[must_use]
    pub fn spec() -> Self {
        Self {
            base_ms: DEFAULT_BASE_MS,
            factor: 2,
            cap_ms: DEFAULT_CAP_MS,
            jitter_fraction: DEFAULT_JITTER_FRACTION,
            max_attempts: DEFAULT_MAX_ATTEMPTS,
            window_ms: DEFAULT_WINDOW_MS,
        }
    }

    /// The same shape with every duration scaled down, for tests that need the
    /// reconnect *logic* without the reconnect *waiting*.
    ///
    /// Keeps `max_attempts` and the window ratio intact so a test cannot
    /// accidentally pass by having a budget the production policy would not
    /// grant it.
    #[must_use]
    pub fn fast() -> Self {
        Self {
            base_ms: 5,
            factor: 2,
            cap_ms: 40,
            jitter_fraction: DEFAULT_JITTER_FRACTION,
            max_attempts: DEFAULT_MAX_ATTEMPTS,
            window_ms: DEFAULT_WINDOW_MS,
        }
    }

    /// The delay for `attempt` before jitter, in milliseconds.
    ///
    /// Attempt 0 is the first retry. Saturating rather than wrapping, so a
    /// pathological attempt count clamps at the cap instead of folding back to a
    /// tight loop.
    #[must_use]
    pub fn unjittered_delay_ms(&self, attempt: u32) -> u64 {
        let mut delay = self.base_ms;
        for _ in 0..attempt {
            delay = delay.saturating_mul(u64::from(self.factor));
            if delay >= self.cap_ms {
                return self.cap_ms;
            }
        }
        delay.min(self.cap_ms)
    }

    /// The delay for `attempt` with `unit` (a draw in `[0.0, 1.0)`) applied as
    /// symmetric jitter.
    ///
    /// `unit = 0.5` is the unjittered delay; `0.0` and `1.0` are the ends of the
    /// ±`jitter_fraction` band.
    #[must_use]
    pub fn delay_ms(&self, attempt: u32, unit: f64) -> u64 {
        let base = self.unjittered_delay_ms(attempt) as f64;
        let unit = unit.clamp(0.0, 1.0);
        let multiplier = 1.0 - self.jitter_fraction + 2.0 * self.jitter_fraction * unit;
        (base * multiplier).round() as u64
    }

    /// A fresh budget for this policy.
    #[must_use]
    pub fn budget(&self) -> ReconnectBudget {
        ReconnectBudget::new(self.max_attempts, self.window_ms)
    }
}

/// The rolling-window attempt counter.
///
/// Times are caller-supplied milliseconds on any monotonic scale — the stream
/// passes its own elapsed-since-open, which keeps the whole thing testable
/// without a clock.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReconnectBudget {
    max_attempts: u32,
    window_ms: u64,
    attempts: VecDeque<u64>,
}

impl ReconnectBudget {
    /// A budget of `max_attempts` inside a `window_ms` rolling window.
    #[must_use]
    pub fn new(max_attempts: u32, window_ms: u64) -> Self {
        Self {
            max_attempts,
            window_ms,
            attempts: VecDeque::new(),
        }
    }

    /// Record an attempt at `now_ms`, returning its zero-based index inside the
    /// current window, or `None` when the budget is exhausted.
    ///
    /// The index doubles as the exponent for [`BackoffPolicy::delay_ms`], so a
    /// stream that has been flapping for a while resumes at the long delays
    /// rather than restarting at 250 ms.
    pub fn try_record(&mut self, now_ms: u64) -> Option<u32> {
        self.prune(now_ms);
        if self.attempts.len() as u32 >= self.max_attempts {
            return None;
        }
        self.attempts.push_back(now_ms);
        Some(self.attempts.len() as u32 - 1)
    }

    /// Attempts still available at `now_ms`, without recording one.
    pub fn remaining(&mut self, now_ms: u64) -> u32 {
        self.prune(now_ms);
        self.max_attempts.saturating_sub(self.attempts.len() as u32)
    }

    /// Forget every recorded attempt.
    ///
    /// Not called on a successful connect on purpose: a socket that opens and
    /// dies immediately would otherwise reset the budget every time and loop
    /// forever. The window is what expires attempts, not success.
    pub fn clear(&mut self) {
        self.attempts.clear();
    }

    fn prune(&mut self, now_ms: u64) {
        // Compared by addition rather than by subtracting the window from
        // `now_ms`: a saturating subtraction near t=0 collapses the cutoff onto
        // zero and expires the very first attempt of the session immediately,
        // which silently doubles the budget for exactly the flapping streams
        // the budget exists to catch.
        while let Some(&oldest) = self.attempts.front() {
            if oldest.saturating_add(self.window_ms) <= now_ms {
                self.attempts.pop_front();
            } else {
                break;
            }
        }
    }
}
