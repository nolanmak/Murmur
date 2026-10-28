//! Real-model acceptance suite for Local Parakeet (#101). Not part of the
//! ordinary tests: it needs the fotw-speechd helper, installed models and the
//! shared corpus. Run it through scripts/eval-local-parakeet.py, which also
//! denies the network and scores the output.
//!
//! parakeet_eval --helper PATH --models DIR --corpus DIR [--trials N]
//!
//! Prints one JSON object per line:
//! - `{"kind":"load", ...}`: helper start, load and warmup time;
//! - `{"kind":"clip", "path", "category", "text", "ms"}`: every corpus clip
//!   through Local Parakeet's preflight and whole-utterance transcription;
//! - `{"kind":"latency", "ms", "loadavg"}`: release to text for a 10 s
//!   utterance through the full capture provider path, one per warm trial.
use murmur::local_parakeet::{Endpoint, Service, SpeechdBackend};
use murmur::local_stt::LocalTranscriber;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const CATEGORIES: [&str; 7] = [
    "speech",
    "speech_in_noise",
    "speech_in_music",
    "music/clips",
    "noise/clips",
    "silence",
    "thank_you",
];

fn main() -> Result<(), String> {
    let args: Vec<String> = std::env::args().collect();
    let value = |flag: &str| {
        args.iter()
            .position(|a| a == flag)
            .and_then(|i| args.get(i + 1))
            .cloned()
    };
    let usage = "Usage: parakeet_eval --helper PATH --models DIR --corpus DIR [--trials N]";
    let helper = PathBuf::from(value("--helper").ok_or(usage)?);
    let models = PathBuf::from(value("--models").ok_or(usage)?);
    let corpus = PathBuf::from(value("--corpus").ok_or(usage)?);
    let trials: usize = value("--trials").map_or(Ok(20), |v| v.parse().map_err(|_| usage))?;

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    runtime.block_on(async move {
        let config = fotw_speechd_client::SpeechdConfig::new(helper, &models);
        let backend = SpeechdBackend::new(
            Ok(config),
            murmur::local_parakeet::ASR_ENGINE,
            Some(murmur::local_parakeet::VAD_ENGINE),
        );
        let endpoint = Endpoint::new(Arc::new(Service::new(Arc::new(backend))?));
        let started = Instant::now();
        let ready = endpoint.prepare("en", Duration::from_secs(300)).await?;
        emit(serde_json::json!({
            "kind": "load",
            "start_load_warm_ms": started.elapsed().as_millis() as u64,
        }));
        let local = LocalTranscriber::Parakeet(ready.clone());

        for category in CATEGORIES {
            let mut clips: Vec<PathBuf> = std::fs::read_dir(corpus.join(category))
                .map_err(|e| format!("{category}: {e}"))?
                .filter_map(Result::ok)
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|x| x == "wav"))
                .collect();
            clips.sort();
            for clip in clips {
                let pcm = read_wav(&clip)?;
                // Same preflight per utterance as a dictation.
                let bound = local.prepare("en", Duration::from_secs(2)).await?;
                let started = Instant::now();
                let text = bound
                    .transcribe(&pcm, Arc::new(AtomicU8::new(0)), Duration::from_secs(180))
                    .await?;
                emit(serde_json::json!({
                    "kind": "clip",
                    "path": clip.strip_prefix(&corpus).unwrap_or(&clip),
                    "category": category,
                    "text": text,
                    "ms": started.elapsed().as_secs_f64() * 1000.0,
                }));
            }
        }

        let utterance = ten_seconds(&corpus)?;
        for trial in 0..trials + 2 {
            let ms = release_to_text(&endpoint, &utterance).await?;
            // The first two runs warm the capture path too; only warm trials count.
            if trial >= 2 {
                emit(serde_json::json!({"kind": "latency", "ms": ms, "loadavg": loadavg()}));
            }
        }
        Ok(())
    })
}

fn emit(value: serde_json::Value) {
    println!("{value}");
}

fn loadavg() -> String {
    std::process::Command::new("sysctl")
        .args(["-n", "vm.loadavg"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

/// Exactly 10 s of LibriSpeech speech: speech clips joined in name order.
fn ten_seconds(corpus: &Path) -> Result<Vec<i16>, String> {
    let mut names: Vec<PathBuf> = std::fs::read_dir(corpus.join("speech"))
        .map_err(|e| e.to_string())?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "wav"))
        .collect();
    names.sort();
    let mut pcm = Vec::new();
    for name in names {
        pcm.extend(read_wav(&name)?);
        if pcm.len() >= 160_000 {
            pcm.truncate(160_000);
            return Ok(pcm);
        }
    }
    Err("not enough speech for a 10 s utterance".into())
}

/// The full capture provider path, as a dictation runs it: preflight, mic
/// start (a fixture tap that delivers the utterance), release, drain,
/// resample, VAD, Parakeet, one line. Measured from release to the text.
async fn release_to_text(endpoint: &Endpoint, utterance: &[i16]) -> Result<f64, String> {
    let control = Arc::new(AtomicU8::new(0));
    let released = Arc::new(Mutex::new(None));
    let tap = FixtureTap {
        id: fotw_audio::TapId::mic("parakeet-eval"),
        samples: utterance.iter().map(|&s| f32::from(s) / 32768.0).collect(),
        control: control.clone(),
        released: released.clone(),
    };
    let text = murmur::capture::run_provider_with_preparation(
        murmur::capture::InputPreparation::new(move || {
            Ok(murmur::capture::PreparedInput::new(Box::new(tap)))
        }),
        murmur::capture::CaptureProvider::Local(endpoint.clone().into()),
        control,
        Arc::new(AtomicBool::new(false)),
        Default::default(),
        Default::default(),
        fotw_audio::device_change::DeviceChangeSignal::new(),
        murmur::vocabulary::Profile::default(),
    )
    .await?;
    let at = released
        .lock()
        .unwrap()
        .ok_or("the fixture never released")?;
    if text.split_whitespace().count() < 10 {
        return Err(format!("unexpected text for the 10 s utterance: {text:?}"));
    }
    Ok(at.elapsed().as_secs_f64() * 1000.0)
}

struct FixtureTap {
    id: fotw_audio::TapId,
    samples: Vec<f32>,
    control: Arc<AtomicU8>,
    released: Arc<Mutex<Option<Instant>>>,
}
impl fotw_audio::AudioTap for FixtureTap {
    fn id(&self) -> &fotw_audio::TapId {
        &self.id
    }
    fn format(&self) -> fotw_audio::StreamFormat {
        fotw_audio::StreamFormat::new(16_000, 1, fotw_audio::SampleFormat::F32)
    }
    fn format_is_authoritative(&self) -> bool {
        true
    }
    fn start(
        &mut self,
        mut sink: Box<dyn fotw_audio::FrameSink>,
    ) -> Result<fotw_audio::StreamFormat, fotw_audio::TapError> {
        sink.on_frames(
            &self.samples,
            fotw_audio::CaptureTimestamp::new(0, 0),
            fotw_audio::FrameFlags::empty(),
        );
        // The key goes up the moment the last sample is captured.
        *self.released.lock().unwrap() = Some(Instant::now());
        self.control.store(1, Ordering::Release);
        Ok(self.format())
    }
    fn stop(&mut self) -> Result<(), fotw_audio::TapError> {
        Ok(())
    }
}

fn read_wav(path: &Path) -> Result<Vec<i16>, String> {
    let wav = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if wav.len() < 12 || &wav[..4] != b"RIFF" || &wav[8..12] != b"WAVE" {
        return Err(format!("{}: not a WAV", path.display()));
    }
    let mut cursor = 12;
    while cursor + 8 <= wav.len() {
        let length = u32::from_le_bytes(wav[cursor + 4..cursor + 8].try_into().unwrap()) as usize;
        let start = cursor + 8;
        let end = (start + length).min(wav.len());
        if &wav[cursor..cursor + 4] == b"fmt " {
            let format = &wav[start..end];
            // PCM, mono, 16 kHz, 16-bit: the corpus format.
            if format.len() < 16
                || format[0..2] != 1u16.to_le_bytes()
                || format[2..4] != 1u16.to_le_bytes()
                || format[4..8] != 16_000u32.to_le_bytes()
                || format[14..16] != 16u16.to_le_bytes()
            {
                return Err(format!("{}: expected 16 kHz mono PCM16", path.display()));
            }
        }
        if &wav[cursor..cursor + 4] == b"data" {
            return Ok(wav[start..end]
                .chunks_exact(2)
                .map(|b| i16::from_le_bytes([b[0], b[1]]))
                .collect());
        }
        cursor = start + length + (length & 1);
    }
    Err(format!("{}: no data chunk", path.display()))
}
