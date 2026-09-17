//! What a tap hands you: samples, when they happened, and what was wrong with
//! them.

use std::fmt;
use std::ops::{
    BitAnd, BitAndAssign, BitOr, BitOrAssign, BitXor, BitXorAssign, Not, Sub, SubAssign,
};

use crate::error::TapError;

/// When a buffer happened, on two clocks.
///
/// `device_frames` is the running frame counter of *this* stream — the only
/// exact measure of how much audio exists. `host_ns` is the shared host clock
/// (see [`crate::clock`]) and is what makes the mic leg and the system leg
/// line up: seam rule 3, and the mechanism behind CAP-03's "reconstruct
/// interleaving within 50 ms".
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct CaptureTimestamp {
    /// Frames delivered by this stream before this buffer. Starts at 0 and
    /// only ever increases; a jump is a gap and must be paired with
    /// [`FrameFlags::DISCONTINUITY`].
    pub device_frames: u64,
    /// Nanoseconds on the process-wide monotonic host clock, stamped at the
    /// tap boundary.
    pub host_ns: u64,
}

impl CaptureTimestamp {
    /// Construct a timestamp.
    #[must_use]
    pub const fn new(device_frames: u64, host_ns: u64) -> Self {
        Self {
            device_frames,
            host_ns,
        }
    }

    /// True when `self` is at or after `earlier` on *both* clocks.
    ///
    /// The pipeline asserts this across consecutive callbacks; a failure means
    /// the backend is stamping from more than one clock, which silently
    /// corrupts two-stream alignment.
    #[must_use]
    pub const fn is_monotonic_after(&self, earlier: &Self) -> bool {
        self.device_frames >= earlier.device_frames && self.host_ns >= earlier.host_ns
    }
}

/// Per-buffer conditions, with the WASAPI-compatible bit values from
/// docs/REQUIREMENTS.md 6.5.
///
/// Hand-rolled rather than pulled from `bitflags` so the seam keeps a single
/// third-party dependency; the surface is the subset of the `bitflags` API the
/// pipeline uses.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct FrameFlags(u32);

impl FrameFlags {
    /// The buffer is silent. `AUDCLNT_BUFFERFLAGS_SILENT`.
    ///
    /// Distinct from "we captured nothing": a real macOS tap that goes
    /// bit-exact zero while output is running is the defect in
    /// docs/REQUIREMENTS.md 6.4, and the watchdog above the seam counts these.
    pub const SILENT: Self = Self(1);
    /// Discontinuity with the previous buffer — a dropped, glitched or
    /// restarted stream. `AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY`.
    pub const DISCONTINUITY: Self = Self(2);
    /// The device timestamp for this buffer could not be trusted; `host_ns`
    /// was derived rather than read.
    pub const TIMESTAMP_ERROR: Self = Self(4);

    const KNOWN: u32 = 0b111;

    /// No flags set.
    #[must_use]
    pub const fn empty() -> Self {
        Self(0)
    }

    /// Every defined flag.
    #[must_use]
    pub const fn all() -> Self {
        Self(Self::KNOWN)
    }

    /// The raw bits.
    #[must_use]
    pub const fn bits(self) -> u32 {
        self.0
    }

    /// Build from raw bits, dropping anything undefined.
    #[must_use]
    pub const fn from_bits_truncate(bits: u32) -> Self {
        Self(bits & Self::KNOWN)
    }

    /// Build from raw bits, or `None` if any undefined bit is set.
    #[must_use]
    pub const fn from_bits(bits: u32) -> Option<Self> {
        if bits & !Self::KNOWN == 0 {
            Some(Self(bits))
        } else {
            None
        }
    }

    /// True when no flag is set.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// True when *every* flag in `other` is set.
    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    /// True when *any* flag in `other` is set.
    #[must_use]
    pub const fn intersects(self, other: Self) -> bool {
        self.0 & other.0 != 0
    }

    /// Set every flag in `other`.
    pub const fn insert(&mut self, other: Self) {
        self.0 |= other.0;
    }

    /// Clear every flag in `other`.
    pub const fn remove(&mut self, other: Self) {
        self.0 &= !other.0;
    }

    /// Set or clear `other` according to `on`.
    pub const fn set(&mut self, other: Self, on: bool) {
        if on {
            self.insert(other);
        } else {
            self.remove(other);
        }
    }
}

impl fmt::Debug for FrameFlags {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("FrameFlags(")?;
        if self.is_empty() {
            f.write_str("0x0)")?;
            return Ok(());
        }
        let mut first = true;
        for (flag, name) in [
            (Self::SILENT, "SILENT"),
            (Self::DISCONTINUITY, "DISCONTINUITY"),
            (Self::TIMESTAMP_ERROR, "TIMESTAMP_ERROR"),
        ] {
            if self.contains(flag) {
                if !first {
                    f.write_str(" | ")?;
                }
                f.write_str(name)?;
                first = false;
            }
        }
        f.write_str(")")
    }
}

impl fmt::Display for FrameFlags {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl BitOr for FrameFlags {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

impl BitOrAssign for FrameFlags {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

impl BitAnd for FrameFlags {
    type Output = Self;
    fn bitand(self, rhs: Self) -> Self {
        Self(self.0 & rhs.0)
    }
}

impl BitAndAssign for FrameFlags {
    fn bitand_assign(&mut self, rhs: Self) {
        self.0 &= rhs.0;
    }
}

impl BitXor for FrameFlags {
    type Output = Self;
    fn bitxor(self, rhs: Self) -> Self {
        Self(self.0 ^ rhs.0)
    }
}

impl BitXorAssign for FrameFlags {
    fn bitxor_assign(&mut self, rhs: Self) {
        self.0 ^= rhs.0;
    }
}

impl Sub for FrameFlags {
    type Output = Self;
    fn sub(self, rhs: Self) -> Self {
        Self(self.0 & !rhs.0)
    }
}

impl SubAssign for FrameFlags {
    fn sub_assign(&mut self, rhs: Self) {
        self.0 &= !rhs.0;
    }
}

impl Not for FrameFlags {
    type Output = Self;
    fn not(self) -> Self {
        Self(!self.0 & Self::KNOWN)
    }
}

impl FromIterator<FrameFlags> for FrameFlags {
    fn from_iter<T: IntoIterator<Item = FrameFlags>>(iter: T) -> Self {
        iter.into_iter().fold(Self::empty(), |a, b| a | b)
    }
}

/// Where captured audio goes.
///
/// Implementations run on the backend's callback thread and are subject to
/// that thread's constraints: on a real Core Audio IOProc there must be no
/// allocation, no locking and no logging on this path (CAP-04). The contract
/// is deliberately "copy into a preallocated ring and return".
///
/// Samples are always interleaved `f32` in `[-1.0, 1.0]`, whatever the
/// device's native [`crate::SampleFormat`] is, so no consumer grows a
/// per-format code path.
pub trait FrameSink: Send {
    /// A buffer of `interleaved_f32.len() / channels` frames arrived.
    fn on_frames(&mut self, interleaved_f32: &[f32], ts: CaptureTimestamp, flags: FrameFlags);

    /// The tap failed. After this the tap may deliver nothing further; the
    /// layer above decides whether to rebuild.
    fn on_error(&mut self, e: TapError);
}

impl<T: FrameSink + ?Sized> FrameSink for Box<T> {
    fn on_frames(&mut self, interleaved_f32: &[f32], ts: CaptureTimestamp, flags: FrameFlags) {
        (**self).on_frames(interleaved_f32, ts, flags);
    }

    fn on_error(&mut self, e: TapError) {
        (**self).on_error(e);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_matches_the_bitflags_shape() {
        assert_eq!(format!("{:?}", FrameFlags::empty()), "FrameFlags(0x0)");
        assert_eq!(format!("{:?}", FrameFlags::SILENT), "FrameFlags(SILENT)");
        assert_eq!(
            format!("{:?}", FrameFlags::all()),
            "FrameFlags(SILENT | DISCONTINUITY | TIMESTAMP_ERROR)"
        );
    }

    #[test]
    fn undefined_bits_are_rejected_or_dropped() {
        assert_eq!(FrameFlags::from_bits(8), None);
        assert_eq!(FrameFlags::from_bits_truncate(9), FrameFlags::SILENT);
        assert_eq!(
            !FrameFlags::SILENT,
            FrameFlags::DISCONTINUITY | FrameFlags::TIMESTAMP_ERROR
        );
    }

    #[test]
    fn insert_remove_and_set_round_trip() {
        let mut f = FrameFlags::empty();
        f.insert(FrameFlags::SILENT);
        f.set(FrameFlags::DISCONTINUITY, true);
        assert_eq!(f, FrameFlags::SILENT | FrameFlags::DISCONTINUITY);
        f.remove(FrameFlags::SILENT);
        f.set(FrameFlags::DISCONTINUITY, false);
        assert!(f.is_empty());
    }

    #[test]
    fn monotonicity_requires_both_clocks_to_advance() {
        let a = CaptureTimestamp::new(480, 10_000);
        assert!(CaptureTimestamp::new(960, 20_000).is_monotonic_after(&a));
        assert!(!CaptureTimestamp::new(0, 20_000).is_monotonic_after(&a));
        assert!(!CaptureTimestamp::new(960, 0).is_monotonic_after(&a));
    }
}
