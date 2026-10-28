//! Murmur #100 acceptance replay: corpus clips through Murmur's real capture
//! pipeline (`capture::run_provider_with_preparation`) as if spoken into the
//! microphone, in real time, with the real `fotw-speechd` helper.
//!
//! ```text
//! streaming_eval --helper <fotw-speechd> --models-root <dir with nemotron-3.5 installed>
//!                --manifest <corpus-v2/manifest.json> [--music-snr <dB>]
//!                [--whisper <http://127.0.0.1:PORT>] [--sandbox]
//! ```
//!
//! A fake microphone delivers each clip's 16 kHz PCM in 10 ms frames at real
//! time. Partials are read from the attempt's `Progress` as the indicator
//! reads them; Control is released when the last frame has been delivered,
//! and release → text is timed to the moment the pipeline returns the text
//! that would be typed (after flush, final, vocabulary corrections and
//! snippet expansion; the native paste itself is not included).
//!
//! `--sandbox` runs the helper under `sandbox-exec` with the network denied.
//! `--whisper` also replays every clip through Local Whisper (batch) for
//! comparison. The first dictation after the helper starts is measured
//! against the fifth. `--launches N` instead starts the helper N times and
//! dictates the first clip of 10 s or more six times after each launch.
//! Prints one JSON report.
use fotw_audio::{
    AudioTap, CaptureTimestamp, FrameFlags, FrameSink, SampleFormat, StreamFormat, TapError, TapId,
};
use murmur::capture::{CaptureProvider, InputPreparation, MicrophoneStartup, PreparedInput};
use murmur::fallback::Progress;
use murmur::local_streaming::{ENGINE, Endpoint, Service, SpeechdBackend};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const SANDBOX: &str = "(version 1)(allow default)(deny network*)";

/// Plays one clip into the pipeline at real time.
struct WavTap {
    id: TapId,
    pcm: Arc<Vec<f32>>,
    /// When the first frame went out.
    started: Arc<Mutex<Option<Instant>>>,
    /// Set once the last frame went out.
    done: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
}
impl AudioTap for WavTap {
    fn id(&self) -> &TapId {
        &self.id
    }
    fn format(&self) -> StreamFormat {
        StreamFormat::new(16_000, 1, SampleFormat::F32)
    }
    fn format_is_authoritative(&self) -> bool {
        true
    }
    fn start(&mut self, mut sink: Box<dyn FrameSink>) -> Result<StreamFormat, TapError> {
        let (pcm, started, done, stop) = (
            self.pcm.clone(),
            self.started.clone(),
            self.done.clone(),
            self.stop.clone(),
        );
        std::thread::spawn(move || {
            let begin = Instant::now();
            *started.lock().unwrap() = Some(begin);
            for (i, frame) in pcm.chunks(160).enumerate() {
                if stop.load(Ordering::Acquire) {
                    return;
                }
                let due = begin + Duration::from_millis(10 * i as u64);
                if let Some(wait) = due.checked_duration_since(Instant::now()) {
                    std::thread::sleep(wait);
                }
                sink.on_frames(frame, CaptureTimestamp::new(0, 0), FrameFlags::empty());
            }
            done.store(true, Ordering::Release);
            // Like a live mic, keep delivering (silence) until stopped.
            while !stop.load(Ordering::Acquire) {
                std::thread::sleep(Duration::from_millis(10));
                sink.on_frames(&[0.0; 160], CaptureTimestamp::new(0, 0), FrameFlags::empty());
            }
        });
        Ok(self.format())
    }
    fn stop(&mut self) -> Result<(), TapError> {
        self.stop.store(true, Ordering::Release);
        Ok(())
    }
}

fn read_wav(path: &Path) -> Vec<f32> {
    let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let mut at = 12;
    while at + 8 <= bytes.len() {
        let size = u32::from_le_bytes(bytes[at + 4..at + 8].try_into().unwrap()) as usize;
        if &bytes[at..at + 4] == b"data" {
            return bytes[at + 8..at + 8 + size]
                .chunks_exact(2)
                .map(|b| f32::from(i16::from_le_bytes([b[0], b[1]])) / 32768.0)
                .collect();
        }
        at += 8 + size + (size & 1);
    }
    panic!("{}: no data chunk", path.display());
}

/// One dictation of `pcm`: partial arrivals on the audio clock, release → text.
async fn dictate(provider: CaptureProvider, pcm: Arc<Vec<f32>>) -> Value {
    let control = Arc::new(AtomicU8::new(0));
    let progress = Progress::default();
    let started = Arc::new(Mutex::new(None::<Instant>));
    let done = Arc::new(AtomicBool::new(false));
    let tap = WavTap {
        id: TapId::mic("replay"),
        pcm: pcm.clone(),
        started: started.clone(),
        done: done.clone(),
        stop: Arc::new(AtomicBool::new(false)),
    };
    let released = Arc::new(Mutex::new(None::<Instant>));
    let watcher = tokio::spawn({
        let (control, progress, started, released) = (
            control.clone(),
            progress.clone(),
            started.clone(),
            released.clone(),
        );
        async move {
            let mut partials = Vec::new();
            loop {
                if let (Some(text), Some(begin)) = (progress.live(), *started.lock().unwrap())
                    && partials.last().is_none_or(|(_, last): &(f64, String)| *last != text)
                {
                    partials.push((begin.elapsed().as_secs_f64(), text));
                }
                if done.load(Ordering::Acquire) && released.lock().unwrap().is_none() {
                    *released.lock().unwrap() = Some(Instant::now());
                    control.store(1, Ordering::Release);
                }
                if control.load(Ordering::Acquire) == 3 {
                    return partials;
                }
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
        }
    });
    let result = murmur::capture::run_provider_with_preparation(
        InputPreparation::new(move || Ok(PreparedInput::new(Box::new(tap)))),
        provider,
        control.clone(),
        Arc::new(AtomicBool::new(false)),
        progress.clone(),
        MicrophoneStartup::default(),
        fotw_audio::device_change::DeviceChangeSignal::new(),
        murmur::vocabulary::Profile::default(),
    )
    .await;
    let finished = Instant::now();
    control.store(3, Ordering::Release);
    let partials = watcher.await.unwrap();
    let release_ms = released
        .lock()
        .unwrap()
        .map(|at| finished.duration_since(at).as_secs_f64() * 1000.0);
    json!({
        "audio_s": pcm.len() as f64 / 16_000.0,
        "partials": partials.iter().map(|(t, text)| json!([t, text])).collect::<Vec<_>>(),
        "release_ms": release_ms,
        "text": result.as_ref().ok(),
        "error": result.as_ref().err(),
        "stage": format!("{:?}", progress.stage()),
    })
}

fn clips(manifest: &Path, music_snr: Option<i64>) -> Vec<(PathBuf, String)> {
    let root = manifest.parent().unwrap();
    let parsed: Value = serde_json::from_slice(&std::fs::read(manifest).unwrap()).unwrap();
    parsed["files"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|f| match music_snr {
            None => f["category"] == "speech",
            Some(snr) => f["category"] == "speech_in_music" && f["snr_db"] == snr,
        })
        .map(|f| {
            (
                root.join(f["path"].as_str().unwrap()),
                f["reference"].as_str().unwrap().to_owned(),
            )
        })
        .collect()
}

fn main() {
    let mut helper = None;
    let mut models_root = None;
    let mut manifest = None;
    let mut music_snr = None;
    let mut whisper = None;
    let mut sandbox = false;
    let mut launches = 0usize;
    let mut args = std::env::args().skip(1);
    while let Some(flag) = args.next() {
        match flag.as_str() {
            "--sandbox" => sandbox = true,
            "--helper" => helper = args.next().map(PathBuf::from),
            "--models-root" => models_root = args.next().map(PathBuf::from),
            "--manifest" => manifest = args.next().map(PathBuf::from),
            "--music-snr" => music_snr = args.next().map(|v| v.parse::<i64>().unwrap()),
            "--whisper" => whisper = args.next(),
            "--launches" => launches = args.next().map(|v| v.parse().unwrap()).unwrap(),
            other => panic!("unknown argument {other}"),
        }
    }
    let helper = std::path::absolute(helper.expect("--helper")).unwrap();
    let models_root = std::path::absolute(models_root.expect("--models-root")).unwrap();
    let manifest = manifest.expect("--manifest");
    let clips = clips(&manifest, music_snr);

    let mut config = fotw_speechd_client::SpeechdConfig::new(&helper, &models_root);
    if sandbox {
        let mut wrapped = vec!["-p".into(), SANDBOX.into(), helper.clone().into_os_string()];
        wrapped.append(&mut config.args);
        config.program = "/usr/bin/sandbox-exec".into();
        config.args = wrapped;
    }
    let new_service = || {
        Arc::new(
            Service::new(Arc::new(SpeechdBackend::new(Ok(config.clone()), ENGINE)))
                .expect("service"),
        )
    };
    if launches > 0 {
        let probe = clips
            .iter()
            .map(|(path, _)| read_wav(path))
            .find(|pcm| pcm.len() >= 160_000)
            .expect("a clip of 10 s or more");
        let probe = Arc::new(probe);
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let mut rows = Vec::new();
        for _ in 0..launches {
            // A fresh service is a fresh helper process, as at app launch.
            let service = new_service();
            let row = runtime.block_on(async {
                let launched = Instant::now();
                service.warm();
                service.ready(Duration::from_secs(300)).await.expect("ready");
                let ready_ms = launched.elapsed().as_secs_f64() * 1000.0;
                let mut runs = Vec::new();
                for _ in 0..6 {
                    let provider = CaptureProvider::Streaming {
                        stream: Endpoint::new(service.clone()),
                        fallback: None,
                    };
                    runs.push(dictate(provider, probe.clone()).await);
                }
                json!({"spawn_to_ready_ms": ready_ms, "runs": runs})
            });
            drop(service);
            std::thread::sleep(Duration::from_millis(500));
            rows.push(row);
        }
        println!("{}", serde_json::to_string_pretty(&json!({"launches": rows})).unwrap());
        runtime.shutdown_background();
        return;
    }
    let service = new_service();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let streaming = || CaptureProvider::Streaming {
        stream: Endpoint::new(service.clone()),
        fallback: None,
    };
    let report = runtime.block_on(async {
        // Launch: spawn, load and warm, as the app does at startup.
        let launched = Instant::now();
        service.warm();
        service
            .ready(Duration::from_secs(300))
            .await
            .expect("nemotron-3.5 loads (install it first)");
        let ready_ms = launched.elapsed().as_secs_f64() * 1000.0;
        // The first dictation after launch against the fifth.
        let probe = Arc::new(read_wav(&clips[0].0));
        let mut launch = Vec::new();
        for _ in 0..6 {
            launch.push(dictate(streaming(), probe.clone()).await);
        }
        let mut rows = Vec::new();
        for (path, reference) in &clips {
            let pcm = Arc::new(read_wav(path));
            let mut row = dictate(streaming(), pcm.clone()).await;
            if let Some(url) = &whisper {
                let local = murmur::local_stt::LocalEndpoint::new(url).unwrap();
                let batch = dictate(CaptureProvider::Local(local), pcm).await;
                row["whisper"] = batch;
            }
            row["wav"] = json!(path);
            row["reference"] = json!(reference);
            eprintln!(
                "{} release {:.0} ms",
                path.file_name().unwrap().to_string_lossy(),
                row["release_ms"].as_f64().unwrap_or(-1.0)
            );
            rows.push(row);
        }
        json!({
            "pipeline": "murmur capture::run_provider_with_preparation",
            "sandboxed_helper": sandbox,
            "spawn_to_ready_ms": ready_ms,
            "launch": launch,
            "clips": rows,
        })
    });
    println!("{}", serde_json::to_string_pretty(&report).unwrap());
    runtime.shutdown_background();
}
