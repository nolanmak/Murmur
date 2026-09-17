//! Downsampling to what the STT providers actually want.
//!
//! Every provider takes 16 kHz mono; the tap gives 48 kHz stereo f32 and the
//! mic gives whatever the device feels like — 44.1 kHz normally, 16 or 24 kHz
//! once a Bluetooth headset engages HFP. So this stage has to handle an
//! arbitrary input rate, not a fixed 3:1 ratio (CAP-07).
//!
//! The acceptance criterion in the spec is anti-aliasing, and it is chosen
//! because the naive implementation *looks* correct: taking every third
//! sample produces plausible audio and silently folds everything above 8 kHz
//! down into the speech band, where it degrades transcription in a way no
//! one would trace back to the resampler.

use murmur::resample::{Downmixer, Resampler16k};

/// Energy at `freq` in `samples`, as a fraction of full scale.
///
/// A single-bin Goertzel rather than a full FFT: one number is wanted, and
/// pulling in an FFT crate for a test assertion is not worth it.
fn tone_level(samples: &[f32], rate: f32, freq: f32) -> f32 {
    let k = 2.0 * std::f32::consts::PI * freq / rate;
    let (mut re, mut im) = (0.0f32, 0.0f32);
    for (n, s) in samples.iter().enumerate() {
        let phase = k * n as f32;
        re += s * phase.cos();
        im += s * phase.sin();
    }
    2.0 * (re * re + im * im).sqrt() / samples.len() as f32
}

fn db(v: f32) -> f32 {
    20.0 * v.max(1e-12).log10()
}

fn sine(rate: u32, freq: f32, seconds: f32, amp: f32) -> Vec<f32> {
    let n = (rate as f32 * seconds) as usize;
    (0..n)
        .map(|i| (2.0 * std::f32::consts::PI * freq * i as f32 / rate as f32).sin() * amp)
        .collect()
}

#[test]
fn a_passband_tone_survives_downsampling() {
    let input = sine(48_000, 1_000.0, 1.0, 0.5);
    let mut r = Resampler16k::new(48_000, 1).unwrap();
    let out = r.process_all(&input).unwrap();

    // 48k -> 16k is 3:1, so a second of input is a second of output.
    let expected = 16_000;
    assert!(
        (out.len() as i64 - expected as i64).abs() < 500,
        "expected ~{expected} frames, got {}",
        out.len()
    );

    let level = tone_level(&out[800..], 16_000.0, 1_000.0);
    assert!(
        db(level) > -12.0,
        "the 1 kHz tone must survive; got {:.1} dBFS",
        db(level)
    );
}

/// The test that separates a real resampler from a decimator.
///
/// 15 kHz is above the 8 kHz Nyquist limit at 16 kHz. A correct
/// implementation filters it out before decimating. A naive one folds it to
/// |16000 - 15000| = 1000 Hz at nearly full amplitude — right in the middle of
/// the speech band, and completely inaudible as a bug.
#[test]
fn an_out_of_band_tone_is_filtered_rather_than_folded_into_the_speech_band() {
    let input = sine(48_000, 15_000.0, 1.0, 0.5);
    let mut r = Resampler16k::new(48_000, 1).unwrap();
    let out = r.process_all(&input).unwrap();

    // Skip the filter's start-up transient.
    let steady = &out[1_600..];
    let alias = tone_level(steady, 16_000.0, 1_000.0);
    let total: f32 = steady.iter().map(|s| s.abs()).sum::<f32>() / steady.len() as f32;

    assert!(
        db(alias) < -60.0,
        "15 kHz folded back to 1 kHz at {:.1} dBFS — the anti-alias filter is \
         missing or ineffective. A decimator would show roughly -6 dBFS here.",
        db(alias)
    );
    assert!(
        db(total) < -40.0,
        "out-of-band energy should be removed, not merely displaced; mean \
         level {:.1} dBFS",
        db(total)
    );
}

#[test]
fn handles_the_rates_a_bluetooth_headset_actually_produces() {
    // 44.1 kHz normal, 24/16 kHz once HFP engages mid-meeting. None of these
    // is an integer ratio to 16 kHz, which is why a fixed 3:1 decimator is
    // not an option.
    for rate in [44_100u32, 32_000, 24_000, 22_050, 16_000] {
        let input = sine(rate, 1_000.0, 0.5, 0.5);
        let mut r = Resampler16k::new(rate, 1).unwrap();
        let out = r.process_all(&input).unwrap();

        let expected = 8_000i64; // 0.5s at 16 kHz
        assert!(
            (out.len() as i64 - expected).abs() < 400,
            "{rate} Hz -> expected ~{expected} frames, got {}",
            out.len()
        );
        let level = tone_level(&out[800..], 16_000.0, 1_000.0);
        assert!(
            db(level) > -12.0,
            "{rate} Hz: 1 kHz tone came out at {:.1} dBFS",
            db(level)
        );
    }
}

#[test]
fn stereo_is_downmixed_to_mono_without_clipping() {
    // Two identical channels must average to the same signal, not sum to
    // double it — summing would clip anything above half scale, and the tap
    // routinely delivers louder than that.
    let mono = sine(48_000, 1_000.0, 0.2, 0.8);
    let mut interleaved = Vec::with_capacity(mono.len() * 2);
    for s in &mono {
        interleaved.push(*s);
        interleaved.push(*s);
    }

    let out = Downmixer::to_mono(&interleaved, 2);
    assert_eq!(out.len(), mono.len());
    for (a, b) in out.iter().zip(&mono) {
        assert!((a - b).abs() < 1e-6, "{a} vs {b}");
    }
    assert!(out.iter().all(|s| s.abs() <= 1.0), "downmix must not clip");
}

#[test]
fn opposite_phase_channels_cancel_rather_than_wrapping() {
    let mut interleaved = Vec::new();
    for i in 0..1000 {
        let v = (i as f32 / 1000.0) - 0.5;
        interleaved.push(v);
        interleaved.push(-v);
    }
    let out = Downmixer::to_mono(&interleaved, 2);
    assert!(
        out.iter().all(|s| s.abs() < 1e-6),
        "equal and opposite channels must cancel"
    );
}

#[test]
fn conversion_to_i16_clamps_instead_of_wrapping() {
    // A tap can deliver above full scale; wrapping turns a loud passage into
    // white noise, which is both audible and untranscribable.
    let pcm = [0.0f32, 1.0, -1.0, 2.5, -2.5, 0.5];
    let out = Downmixer::to_i16(&pcm);
    assert_eq!(out[0], 0);
    assert_eq!(out[1], 32_767);
    assert_eq!(out[2], -32_767);
    assert_eq!(out[3], 32_767, "clamped, not wrapped to a negative");
    assert_eq!(out[4], -32_767);
    assert!((out[5] - 16_383).abs() <= 1);
}

#[test]
fn a_resampler_reports_its_output_rate_and_channel_count() {
    let r = Resampler16k::new(48_000, 2).unwrap();
    assert_eq!(r.output_rate(), 16_000);
    assert_eq!(r.channels(), 2);
}

#[test]
fn an_implausible_input_rate_is_a_typed_error() {
    assert!(Resampler16k::new(0, 1).is_err());
}
