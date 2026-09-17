//! Self-describing PCM, so the mock provider transcribes *audio* rather than
//! counting bytes.
//!
//! Every millisecond of test audio carries its own session position in the first
//! two samples. The mock decodes that position and looks up what was said then,
//! which makes its transcription a pure function of the bytes it received —
//! exactly like the real thing. It is the only way the replay path can be tested
//! honestly: replayed audio *must* produce the same words again, and a mock that
//! emitted from an internal cursor would produce different words and hide the
//! duplicate the deduplicator exists to remove.

/// The pipeline's canonical rate.
pub const SAMPLE_RATE: u32 = 16_000;

/// Samples in one millisecond at [`SAMPLE_RATE`].
pub const SAMPLES_PER_MS: usize = 16;

/// 15 bits per sample keeps every stamp non-negative, so a sign-extension bug
/// cannot quietly turn into a plausible-looking timestamp.
const STAMP_BITS: u32 = 15;
const STAMP_MASK: u64 = (1 << STAMP_BITS) - 1;

/// Generate `duration_ms` of stamped 16 kHz mono PCM starting at `start_ms`.
pub fn stamped_pcm(start_ms: u64, duration_ms: u64) -> Vec<i16> {
    let mut samples = vec![0i16; duration_ms as usize * SAMPLES_PER_MS];
    for offset in 0..duration_ms {
        let position = start_ms + offset;
        let block = offset as usize * SAMPLES_PER_MS;
        samples[block] = (position & STAMP_MASK) as i16;
        samples[block + 1] = ((position >> STAMP_BITS) & STAMP_MASK) as i16;
    }
    samples
}

/// Decode the session positions carried by whole millisecond-blocks.
///
/// A trailing partial block is left to the caller to buffer: frame boundaries
/// and millisecond boundaries do not have to line up, and on the replay path
/// they deliberately do not.
pub fn decode_stamps(samples: &[i16]) -> Vec<u64> {
    samples
        .chunks_exact(SAMPLES_PER_MS)
        .map(|block| (block[0] as u64) | ((block[1] as u64) << STAMP_BITS))
        .collect()
}

/// Reinterpret little-endian bytes as 16-bit samples.
pub fn from_linear16_le(bytes: &[u8]) -> Vec<i16> {
    bytes
        .chunks_exact(2)
        .map(|pair| i16::from_le_bytes([pair[0], pair[1]]))
        .collect()
}
