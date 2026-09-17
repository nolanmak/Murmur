//! Stream formats, and the difference between what you asked for and what you
//! got (seam rule 1).

use std::fmt;

/// The sample encoding a device natively produces.
///
/// This describes the *device*, not the delivery: [`crate::FrameSink`] always
/// receives interleaved `f32` regardless of what is here, so the layer above
/// never writes a per-format code path. It is still worth carrying, because it
/// is the difference between "48 kHz f32 from a Core Audio tap" and "fixed
/// 44.1 kHz S16 from Windows process loopback" and the resampler wants to log
/// what it was handed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum SampleFormat {
    /// 16-bit signed integer, little-endian.
    I16,
    /// 32-bit signed integer, little-endian.
    I32,
    /// 32-bit IEEE float.
    F32,
}

impl SampleFormat {
    /// Bytes per sample (per channel) in the device's native encoding.
    #[must_use]
    pub const fn bytes_per_sample(self) -> usize {
        match self {
            Self::I16 => 2,
            Self::I32 | Self::F32 => 4,
        }
    }
}

impl fmt::Display for SampleFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::I16 => "i16",
            Self::I32 => "i32",
            Self::F32 => "f32",
        };
        f.write_str(s)
    }
}

/// What a stream actually is: rate, channel count, native sample encoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StreamFormat {
    /// Frames per second. Never assume 48 kHz — re-read this after every
    /// rebuild (CAP-07).
    pub sample_rate_hz: u32,
    /// Interleaved channel count.
    pub channels: u16,
    /// The device's native sample encoding. Frames are still delivered as
    /// `f32`; see [`SampleFormat`].
    pub sample: SampleFormat,
}

impl StreamFormat {
    /// Construct a format.
    #[must_use]
    pub const fn new(sample_rate_hz: u32, channels: u16, sample: SampleFormat) -> Self {
        Self {
            sample_rate_hz,
            channels,
            sample,
        }
    }

    /// True when the values could describe a real stream.
    ///
    /// A format that fails this is a bug in a backend, not a user error: it
    /// means we read an uninitialised ASBD or a device answered with zeros.
    #[must_use]
    pub const fn is_plausible(&self) -> bool {
        self.sample_rate_hz >= 4_000 && self.sample_rate_hz <= 768_000 && self.channels > 0
    }

    /// Frames in a buffer of `samples` interleaved samples.
    ///
    /// Returns `None` if the buffer is not a whole number of frames, which is
    /// a backend bug worth surfacing rather than rounding away.
    #[must_use]
    pub fn frames_in(&self, samples: usize) -> Option<usize> {
        let ch = usize::from(self.channels);
        if ch == 0 || !samples.is_multiple_of(ch) {
            return None;
        }
        Some(samples / ch)
    }

    /// Nanoseconds of audio `frames` frames represent at this rate.
    #[must_use]
    pub fn frames_to_ns(&self, frames: u64) -> u64 {
        if self.sample_rate_hz == 0 {
            return 0;
        }
        frames.saturating_mul(1_000_000_000) / u64::from(self.sample_rate_hz)
    }
}

impl fmt::Display for StreamFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} Hz / {} ch / {}",
            self.sample_rate_hz, self.channels, self.sample
        )
    }
}

/// A *hint* passed to `open_*`. Every field is optional and every field may be
/// ignored by the platform.
///
/// Seam rule 1: this is a request, not a contract. Windows process loopback is
/// fixed at 44.1 kHz / 2 ch / S16 because `GetMixFormat` returns `E_NOTIMPL`
/// on that client, and macOS hands back whatever the ASBD says. Read
/// [`crate::AudioTap::format`] after `start()` to learn what you got, and use
/// [`FormatRequest::matches`] if you need to know whether the hint survived.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct FormatRequest {
    /// Preferred frames per second.
    pub sample_rate_hz: Option<u32>,
    /// Preferred channel count.
    pub channels: Option<u16>,
    /// Preferred native sample encoding.
    pub sample: Option<SampleFormat>,
}

impl FormatRequest {
    /// "Whatever the device likes" — the right default, because converting is
    /// the pipeline's job and a device that has to convert internally usually
    /// converts worse.
    #[must_use]
    pub const fn any() -> Self {
        Self {
            sample_rate_hz: None,
            channels: None,
            sample: None,
        }
    }

    /// Ask for a sample rate.
    #[must_use]
    pub const fn with_sample_rate(mut self, hz: u32) -> Self {
        self.sample_rate_hz = Some(hz);
        self
    }

    /// Ask for a channel count.
    #[must_use]
    pub const fn with_channels(mut self, channels: u16) -> Self {
        self.channels = Some(channels);
        self
    }

    /// Ask for a native sample encoding.
    #[must_use]
    pub const fn with_sample(mut self, sample: SampleFormat) -> Self {
        self.sample = Some(sample);
        self
    }

    /// True when `format` satisfies every field that was actually requested.
    ///
    /// `FormatRequest::any().matches(_)` is always true.
    #[must_use]
    pub fn matches(&self, format: &StreamFormat) -> bool {
        self.sample_rate_hz
            .is_none_or(|r| r == format.sample_rate_hz)
            && self.channels.is_none_or(|c| c == format.channels)
            && self.sample.is_none_or(|s| s == format.sample)
    }

    /// Fill in every unspecified field from `fallback`.
    ///
    /// Backends use this to turn a partial hint into a concrete format to
    /// attempt; it is *not* an authoritative format until `start()` has run.
    #[must_use]
    pub fn resolve(&self, fallback: StreamFormat) -> StreamFormat {
        StreamFormat {
            sample_rate_hz: self.sample_rate_hz.unwrap_or(fallback.sample_rate_hz),
            channels: self.channels.unwrap_or(fallback.channels),
            sample: self.sample.unwrap_or(fallback.sample),
        }
    }
}

impl From<StreamFormat> for FormatRequest {
    fn from(f: StreamFormat) -> Self {
        Self {
            sample_rate_hz: Some(f.sample_rate_hz),
            channels: Some(f.channels),
            sample: Some(f.sample),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn any_matches_everything_and_a_full_request_does_not() {
        let f = StreamFormat::new(48_000, 2, SampleFormat::F32);
        assert!(FormatRequest::any().matches(&f));
        assert!(FormatRequest::any().with_sample_rate(48_000).matches(&f));
        assert!(!FormatRequest::any().with_sample_rate(16_000).matches(&f));
        assert!(!FormatRequest::from(StreamFormat::new(44_100, 2, SampleFormat::I16)).matches(&f));
    }

    #[test]
    fn resolve_fills_only_the_gaps() {
        let fallback = StreamFormat::new(48_000, 2, SampleFormat::F32);
        let got = FormatRequest::any().with_channels(1).resolve(fallback);
        assert_eq!(got, StreamFormat::new(48_000, 1, SampleFormat::F32));
    }

    #[test]
    fn frames_in_rejects_partial_frames() {
        let f = StreamFormat::new(48_000, 2, SampleFormat::F32);
        assert_eq!(f.frames_in(480), Some(240));
        assert_eq!(f.frames_in(481), None);
    }

    #[test]
    fn implausible_formats_are_rejected() {
        assert!(StreamFormat::new(48_000, 2, SampleFormat::F32).is_plausible());
        assert!(!StreamFormat::new(0, 2, SampleFormat::F32).is_plausible());
        assert!(!StreamFormat::new(48_000, 0, SampleFormat::F32).is_plausible());
    }

    #[test]
    fn frames_to_ns_is_exact_at_common_rates() {
        let f = StreamFormat::new(48_000, 2, SampleFormat::F32);
        assert_eq!(f.frames_to_ns(48_000), 1_000_000_000);
        assert_eq!(f.frames_to_ns(480), 10_000_000);
    }
}
