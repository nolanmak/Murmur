//! Getting captured audio into the shape every STT provider wants.
//!
//! The tap delivers 48 kHz stereo `f32`; the mic delivers whatever the device
//! is doing, which is 44.1 kHz normally and 16 or 24 kHz once a Bluetooth
//! headset engages HFP mid-meeting. Every provider wants **16 kHz mono
//! 16-bit LE**. So this stage handles an arbitrary input rate rather than a
//! fixed 3:1 ratio, and re-reads that rate whenever the device changes
//! (CAP-07).
//!
//! # Why not just take every third sample
//!
//! Because it looks like it works. Decimating without a low-pass filter folds
//! everything above the new 8 kHz Nyquist limit straight back down into the
//! speech band: a 15 kHz component reappears at 1 kHz at nearly full
//! amplitude. The result is plausible-sounding audio with degraded
//! transcription that nobody would trace back to the resampler. `rubato`'s
//! `Fft` resampler applies a proper anti-aliasing window, and
//! `tests/resample.rs` asserts the difference — the naive version fails by
//! about 54 dB.
//!
//! # rubato 5.0
//!
//! Note the API changed completely in 5.0 (released 2026-08-10):
//! `SincFixedIn`/`FftFixedIn` no longer exist, buffers go through the
//! `audioadapter` traits, and `Fft::new` no longer takes `sub_chunks`. Any
//! pre-2026 snippet found online is dead code.

use rubato::audioadapter_buffers::direct::InterleavedSlice;
use rubato::{Fft, FixedSync, Resampler};

/// The rate every provider takes.
pub const TARGET_RATE: u32 = 16_000;

/// Frames per processing chunk.
///
/// 10 ms at the target rate. The whole pipeline is driven in 10 ms units
/// because the echo canceller *panics* rather than errors on any other size,
/// so lining up here means the framing downstream is free.
const CHUNK_FRAMES: usize = 160;

/// Errors from constructing or running a resampler.
#[derive(Debug, thiserror::Error)]
pub enum ResampleError {
    /// The input rate could not describe a real stream.
    #[error("implausible input sample rate: {0} Hz")]
    BadRate(u32),
    /// The channel count could not describe a real stream.
    #[error("implausible channel count: {0}")]
    BadChannels(u16),
    /// The resampler itself failed.
    #[error("resampler failed: {0}")]
    Backend(String),
}

/// Resamples an arbitrary input rate to 16 kHz.
pub struct Resampler16k {
    inner: Fft<f32>,
    input_rate: u32,
    channels: u16,
    /// Interleaved input frames not yet consumed. The resampler wants
    /// fixed-size chunks; the tap delivers whatever Core Audio felt like, so
    /// the remainder is carried rather than dropped.
    pending: Vec<f32>,
}

impl std::fmt::Debug for Resampler16k {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Resampler16k")
            .field("input_rate", &self.input_rate)
            .field("channels", &self.channels)
            .field("pending_samples", &self.pending.len())
            .finish()
    }
}

impl Resampler16k {
    /// Build a resampler from `input_rate` to 16 kHz.
    pub fn new(input_rate: u32, channels: u16) -> Result<Self, ResampleError> {
        if !(4_000..=768_000).contains(&input_rate) {
            return Err(ResampleError::BadRate(input_rate));
        }
        if channels == 0 {
            return Err(ResampleError::BadChannels(channels));
        }

        // Fixed OUTPUT size: downstream wants a steady 160-frame cadence, and
        // letting the output vary would push the re-blocking problem into the
        // AEC, which cannot tolerate it.
        let inner = Fft::<f32>::new(
            input_rate as usize,
            TARGET_RATE as usize,
            CHUNK_FRAMES,
            channels as usize,
            FixedSync::Output,
        )
        .map_err(|e| ResampleError::Backend(e.to_string()))?;

        Ok(Self {
            inner,
            input_rate,
            channels,
            pending: Vec::new(),
        })
    }

    /// The input rate this was built for.
    #[must_use]
    pub const fn input_rate(&self) -> u32 {
        self.input_rate
    }

    /// Always 16 kHz.
    #[must_use]
    pub const fn output_rate(&self) -> u32 {
        TARGET_RATE
    }

    /// Channel count.
    #[must_use]
    pub const fn channels(&self) -> u16 {
        self.channels
    }

    /// Resample an entire interleaved buffer, returning interleaved output.
    ///
    /// Any input frames left over at the end are carried into the next call,
    /// so streaming a meeting through repeated calls loses nothing at the
    /// boundaries.
    pub fn process_all(&mut self, interleaved: &[f32]) -> Result<Vec<f32>, ResampleError> {
        let ch = self.channels as usize;
        self.pending.extend_from_slice(interleaved);

        let mut out = Vec::new();
        loop {
            let needed = self.inner.input_frames_next();
            if self.pending.len() < needed * ch {
                break;
            }

            let taken: Vec<f32> = self.pending.drain(..needed * ch).collect();
            let adapter = InterleavedSlice::new(taken.as_slice(), ch, needed)
                .map_err(|e| ResampleError::Backend(e.to_string()))?;

            let produced = self
                .inner
                .process(&adapter, None)
                .map_err(|e| ResampleError::Backend(e.to_string()))?;

            // `take_data` hands back the interleaved buffer directly, which is
            // already the layout everything downstream wants.
            out.extend_from_slice(&produced.take_data());
        }
        Ok(out)
    }
}

/// Channel folding and sample-format conversion.
///
/// Separate from the resampler because the order matters and is not obvious:
/// downmix *after* resampling would run the filter over twice the data for no
/// benefit, and converting to `i16` before resampling would quantise twice.
#[derive(Debug)]
pub struct Downmixer;

impl Downmixer {
    /// Average interleaved channels down to mono.
    ///
    /// Averaged, not summed. Summing two correlated channels doubles the
    /// amplitude and clips anything above half scale, which the tap delivers
    /// routinely.
    #[must_use]
    pub fn to_mono(interleaved: &[f32], channels: u16) -> Vec<f32> {
        let ch = channels.max(1) as usize;
        if ch == 1 {
            return interleaved.to_vec();
        }
        interleaved
            .chunks_exact(ch)
            .map(|frame| frame.iter().sum::<f32>() / ch as f32)
            .collect()
    }

    /// Convert to 16-bit little-endian samples, clamping rather than wrapping.
    ///
    /// A tap can deliver above full scale. Wrapping turns a loud passage into
    /// white noise, which is both audible and untranscribable; clamping merely
    /// flattens the peak.
    #[must_use]
    pub fn to_i16(pcm: &[f32]) -> Vec<i16> {
        pcm.iter()
            .map(|s| (s.clamp(-1.0, 1.0) * 32_767.0) as i16)
            .collect()
    }

    /// Little-endian bytes, ready for a provider socket.
    #[must_use]
    pub fn to_le_bytes(pcm: &[f32]) -> Vec<u8> {
        let mut out = Vec::with_capacity(pcm.len() * 2);
        for s in Self::to_i16(pcm) {
            out.extend_from_slice(&s.to_le_bytes());
        }
        out
    }
}
