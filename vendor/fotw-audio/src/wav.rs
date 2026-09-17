//! A deliberately tiny RIFF/WAVE reader for test fixtures.
//!
//! Hand-rolled rather than pulling a WAV crate: the seam keeps its dependency
//! surface near zero, and fixtures only ever need 16-bit or 32-bit-float PCM.

use std::path::Path;

use crate::error::TapError;
use crate::format::{SampleFormat, StreamFormat};

/// Decoded fixture audio, always converted to interleaved `f32`.
#[derive(Debug, Clone, PartialEq)]
pub struct WavData {
    /// The format as declared by the file's `fmt ` chunk.
    pub format: StreamFormat,
    /// Interleaved samples in `[-1.0, 1.0]`.
    pub samples: Vec<f32>,
}

impl WavData {
    /// Total frames.
    #[must_use]
    pub fn frames(&self) -> usize {
        self.format.frames_in(self.samples.len()).unwrap_or(0)
    }
}

fn u16le(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}

fn u32le(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

/// Parse a RIFF/WAVE byte buffer.
///
/// Supports PCM (format tag 1) at 16 bits and IEEE float (tag 3) at 32 bits,
/// which covers every fixture we generate and the two formats Core Audio and
/// WASAPI actually hand us.
pub fn parse(bytes: &[u8]) -> Result<WavData, TapError> {
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err(TapError::platform("not a RIFF/WAVE file"));
    }

    let mut pos = 12;
    let mut format: Option<(StreamFormat, u16)> = None;
    let mut data: Option<&[u8]> = None;

    while pos + 8 <= bytes.len() {
        let id = &bytes[pos..pos + 4];
        let size = u32le(bytes, pos + 4) as usize;
        let body_start = pos + 8;
        let body_end = body_start.saturating_add(size).min(bytes.len());
        let body = &bytes[body_start..body_end];

        match id {
            b"fmt " => {
                if body.len() < 16 {
                    return Err(TapError::platform("truncated fmt chunk"));
                }
                let tag = u16le(body, 0);
                let channels = u16le(body, 2);
                let rate = u32le(body, 4);
                let bits = u16le(body, 14);
                let sample = match (tag, bits) {
                    (1, 16) => SampleFormat::I16,
                    (3, 32) => SampleFormat::F32,
                    _ => {
                        return Err(TapError::platform(format!(
                            "unsupported WAV encoding: tag {tag}, {bits} bits"
                        )));
                    }
                };
                format = Some((StreamFormat::new(rate, channels, sample), bits));
            }
            b"data" => data = Some(body),
            _ => {}
        }

        // RIFF chunks are word-aligned; an odd size is followed by a pad byte.
        pos = body_start + size + (size & 1);
    }

    let (format, bits) = format.ok_or_else(|| TapError::platform("WAV has no fmt chunk"))?;
    let data = data.ok_or_else(|| TapError::platform("WAV has no data chunk"))?;

    let samples = match (format.sample, bits) {
        (SampleFormat::I16, 16) => data
            .chunks_exact(2)
            .map(|c| f32::from(i16::from_le_bytes([c[0], c[1]])) / 32_768.0)
            .collect(),
        (SampleFormat::F32, 32) => data
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect(),
        _ => return Err(TapError::platform("unsupported WAV sample width")),
    };

    Ok(WavData { format, samples })
}

/// Read and parse a WAV file.
pub fn read(path: impl AsRef<Path>) -> Result<WavData, TapError> {
    let path = path.as_ref();
    let bytes =
        std::fs::read(path).map_err(|e| TapError::io(format!("reading {}", path.display()), e))?;
    parse(&bytes)
}

/// Encode interleaved `f32` as a 16-bit PCM WAV. Used to build fixtures.
#[must_use]
pub fn encode_i16(format: StreamFormat, samples: &[f32]) -> Vec<u8> {
    let data_len = samples.len() * 2;
    let mut out = Vec::with_capacity(44 + data_len);
    let byte_rate = format.sample_rate_hz * u32::from(format.channels) * 2;
    let block_align = format.channels * 2;

    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&((36 + data_len) as u32).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&format.channels.to_le_bytes());
    out.extend_from_slice(&format.sample_rate_hz.to_le_bytes());
    out.extend_from_slice(&byte_rate.to_le_bytes());
    out.extend_from_slice(&block_align.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&(data_len as u32).to_le_bytes());
    for s in samples {
        let v = (s.clamp(-1.0, 1.0) * 32_767.0).round() as i16;
        out.extend_from_slice(&v.to_le_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_then_parse_round_trips() {
        let format = StreamFormat::new(16_000, 1, SampleFormat::I16);
        let samples: Vec<f32> = (0..160).map(|i| (i as f32 / 160.0) - 0.5).collect();
        let parsed = parse(&encode_i16(format, &samples)).unwrap();

        assert_eq!(parsed.format, format);
        assert_eq!(parsed.frames(), 160);
        for (a, b) in samples.iter().zip(&parsed.samples) {
            assert!((a - b).abs() < 1.0 / 32_767.0, "{a} vs {b}");
        }
    }

    #[test]
    fn a_non_riff_buffer_is_a_typed_error_not_a_panic() {
        assert!(parse(b"definitely not a wav").is_err());
        assert!(parse(&[]).is_err());
    }
}
