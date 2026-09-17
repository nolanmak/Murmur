//! The gapless-replay PCM ring (spec 4.2, STT-09).
//!
//! Everything written to a provider is also kept here for 30 seconds, so a
//! dropped socket can be re-fed from the last finalized word rather than from
//! wherever the new socket happens to open. Without it a reconnect silently
//! loses however long the outage lasted, which is the worst possible failure for
//! a recorder: the transcript looks complete and simply is not.
//!
//! Position is tracked in **samples**, not milliseconds, and milliseconds are
//! derived. Tracking milliseconds directly accumulates rounding error on every
//! write, and after an hour of 10 ms buffers that error is a visible timestamp
//! drift.

use std::collections::VecDeque;

/// The pipeline's canonical capture rate (spec 7.4: `sample_rate=16000`).
pub const DEFAULT_SAMPLE_RATE: u32 = 16_000;

/// The replay window STT-09 specifies, in milliseconds.
pub const DEFAULT_WINDOW_MS: u64 = 30_000;

#[derive(Debug, Clone)]
struct Chunk {
    start_sample: u64,
    samples: Vec<i16>,
}

impl Chunk {
    fn end_sample(&self) -> u64 {
        self.start_sample + self.samples.len() as u64
    }
}

/// A bounded, time-indexed buffer of the 16-bit mono PCM most recently sent.
///
/// Not a fixed-capacity ring in the lock-free sense — this sits on the async
/// side of the pipeline, well away from the real-time audio thread, where a
/// `VecDeque` of the caller's own buffers is both simpler and cheaper than
/// copying into a preallocated arena.
#[derive(Debug, Clone)]
pub struct PcmRing {
    sample_rate: u32,
    window_samples: u64,
    chunks: VecDeque<Chunk>,
    written_samples: u64,
}

impl PcmRing {
    /// A ring holding `window_ms` of audio at `sample_rate`.
    #[must_use]
    pub fn new(sample_rate: u32, window_ms: u64) -> Self {
        let sample_rate = sample_rate.max(1);
        Self {
            sample_rate,
            window_samples: window_ms.saturating_mul(u64::from(sample_rate)) / 1_000,
            chunks: VecDeque::new(),
            written_samples: 0,
        }
    }

    /// The STT-09 default: 30 s at 16 kHz.
    #[must_use]
    pub fn spec() -> Self {
        Self::new(DEFAULT_SAMPLE_RATE, DEFAULT_WINDOW_MS)
    }

    /// Append `samples` and return the new write position in session
    /// milliseconds.
    ///
    /// The ring is the audio clock: the stream's notion of "how far into the
    /// session are we" is exactly how much PCM it has handed over, which needs
    /// no wall clock and cannot drift away from the audio the provider actually
    /// heard.
    pub fn push(&mut self, samples: &[i16]) -> u64 {
        if !samples.is_empty() {
            self.chunks.push_back(Chunk {
                start_sample: self.written_samples,
                samples: samples.to_vec(),
            });
            self.written_samples += samples.len() as u64;
            self.evict();
        }
        self.written_ms()
    }

    /// Session milliseconds of audio written so far.
    #[must_use]
    pub fn written_ms(&self) -> u64 {
        self.samples_to_ms(self.written_samples)
    }

    /// The oldest session millisecond still replayable.
    #[must_use]
    pub fn earliest_ms(&self) -> u64 {
        self.samples_to_ms(self.earliest_sample())
    }

    /// How much audio is currently retained, in milliseconds.
    #[must_use]
    pub fn buffered_ms(&self) -> u64 {
        self.written_ms().saturating_sub(self.earliest_ms())
    }

    /// Whether anything is retained.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.chunks.is_empty()
    }

    /// Audio from `session_ms` to the write head.
    ///
    /// Returns the position the replay actually starts at, which is `session_ms`
    /// clamped into the retained window. **The caller must use the returned
    /// position, not the one it asked for**, to rebase
    /// [`SessionClock`](crate::SessionClock): they differ exactly when the
    /// outage outlived the ring, and using the requested position there shifts
    /// every subsequent timestamp by the difference.
    #[must_use]
    pub fn replay_from(&self, session_ms: u64) -> Replay {
        let requested = self.ms_to_samples(session_ms);
        let start = requested.clamp(self.earliest_sample(), self.written_samples);

        let mut samples = Vec::new();
        for chunk in &self.chunks {
            if chunk.end_sample() <= start {
                continue;
            }
            let offset = start.saturating_sub(chunk.start_sample) as usize;
            samples.extend_from_slice(&chunk.samples[offset..]);
        }

        Replay {
            start_ms: self.samples_to_ms(start),
            truncated_ms: self.samples_to_ms(start.saturating_sub(requested)),
            samples,
        }
    }

    /// Drop everything. Used after a replay whose audio the provider has now
    /// heard twice is no longer interesting — but note the stream does *not* do
    /// this, because the next outage needs the same 30 s window.
    pub fn clear(&mut self) {
        self.chunks.clear();
    }

    fn earliest_sample(&self) -> u64 {
        self.chunks
            .front()
            .map_or(self.written_samples, |chunk| chunk.start_sample)
    }

    fn evict(&mut self) {
        let cutoff = self.written_samples.saturating_sub(self.window_samples);
        while let Some(front) = self.chunks.front() {
            if front.end_sample() <= cutoff {
                self.chunks.pop_front();
            } else {
                break;
            }
        }
    }

    fn samples_to_ms(&self, samples: u64) -> u64 {
        samples.saturating_mul(1_000) / u64::from(self.sample_rate)
    }

    fn ms_to_samples(&self, ms: u64) -> u64 {
        ms.saturating_mul(u64::from(self.sample_rate)) / 1_000
    }
}

/// What [`PcmRing::replay_from`] found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Replay {
    /// Session position of the first replayed sample, after clamping.
    pub start_ms: u64,
    /// How much of the requested range had already been evicted.
    ///
    /// Non-zero means the outage outlived the ring and that much audio will
    /// never be transcribed live. STT-10's re-transcribe-from-the-recording
    /// path is the only way to recover it, so this is worth surfacing rather
    /// than swallowing.
    pub truncated_ms: u64,
    /// The PCM to replay, in the same 16-bit mono format it was written as.
    pub samples: Vec<i16>,
}

impl Replay {
    /// Whether there is anything to send.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }

    /// Whether audio was lost to eviction.
    #[must_use]
    pub fn lost_audio(&self) -> bool {
        self.truncated_ms > 0
    }
}

/// Encode 16-bit mono samples as the little-endian bytes Deepgram's
/// `encoding=linear16` expects.
#[must_use]
pub fn to_linear16_le(samples: &[i16]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(samples.len() * 2);
    for sample in samples {
        bytes.extend_from_slice(&sample.to_le_bytes());
    }
    bytes
}
