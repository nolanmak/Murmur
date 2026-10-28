//! Murmur #100: local streaming dictation, against a fake streaming engine.
//! No model, helper, microphone or network: a scripted [`Backend`] stands in
//! for `fotw-speechd`, and a fake tap delivers 10 ms frames in real time.
use fotw_audio::{
    AudioTap, CaptureTimestamp, FrameFlags, FrameSink, SampleFormat, StreamFormat, TapError, TapId,
};
use murmur::capture::{CaptureProvider, InputPreparation, MicrophoneStartup, PreparedInput};
use murmur::fallback::{Progress, Stage};
use murmur::local_streaming::{
    Backend, Binding, BoxFuture, Endpoint, Event, LiveStream, LoadError, Service,
};
use murmur::speech::LocalEngine;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

// ------------------------------------------------------------ fake engine

/// What the fake stream does with audio.
#[derive(Clone)]
enum Script {
    /// A partial "heard <n>" every 1600 samples; the final is `final_text`.
    Speak { final_text: String },
    /// Fails once this many samples have arrived.
    FailAfter(usize),
}

#[derive(Default)]
struct Log {
    opened: usize,
    samples: usize,
    finished: bool,
    dropped: bool,
    languages: Vec<Option<String>>,
    /// Samples each stream received, in opening order.
    per_stream: Vec<usize>,
    /// Loads and unloads the backend was asked for.
    loads: usize,
    unloads: usize,
    /// The distinct sample values each stream received.
    values: Vec<std::collections::BTreeSet<i16>>,
}

struct FakeBackend {
    script: Mutex<Script>,
    load: Mutex<Result<(), LoadError>>,
    /// When set, the helper "restarts" (a new instance) once a stream has
    /// received this many samples.
    restart_after: Mutex<Option<usize>>,
    instance: Arc<AtomicU64>,
    log: Arc<Mutex<Log>>,
    /// Lets a test push events into the newest stream after it has ended.
    late: Arc<Mutex<Option<tokio::sync::mpsc::UnboundedSender<Event>>>>,
}

impl FakeBackend {
    fn new(script: Script) -> Arc<Self> {
        Arc::new(Self {
            script: Mutex::new(script),
            load: Mutex::new(Ok(())),
            restart_after: Mutex::new(None),
            instance: Arc::new(AtomicU64::new(1)),
            log: Arc::default(),
            late: Arc::default(),
        })
    }
    fn log(&self) -> std::sync::MutexGuard<'_, Log> {
        self.log.lock().unwrap()
    }
}

impl Backend for FakeBackend {
    fn load(&self) -> BoxFuture<'static, Result<Binding, LoadError>> {
        self.log.lock().unwrap().loads += 1;
        let result = self.load.lock().unwrap().clone().map(|()| Binding {
            instance: self.instance.load(Ordering::SeqCst),
            model: "nemotron-3.5".into(),
            revision: "rev1".into(),
            languages: vec!["en".into(), "de".into()],
        });
        Box::pin(async move { result })
    }
    fn instance(&self) -> Option<u64> {
        Some(self.instance.load(Ordering::SeqCst))
    }
    fn open(
        &self,
        language: Option<String>,
    ) -> BoxFuture<'static, Result<Box<dyn LiveStream>, String>> {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        *self.late.lock().unwrap() = Some(tx.clone());
        let mut log = self.log.lock().unwrap();
        log.opened += 1;
        log.samples = 0;
        log.finished = false;
        log.dropped = false;
        log.languages.push(language);
        log.per_stream.push(0);
        log.values.push(Default::default());
        drop(log);
        let stream = FakeStream {
            restart: self
                .restart_after
                .lock()
                .unwrap()
                .map(|after| (after, self.instance.clone())),
            script: self.script.lock().unwrap().clone(),
            log: self.log.clone(),
            samples: 0,
            tx,
            rx,
        };
        Box::pin(async move { Ok(Box::new(stream) as Box<dyn LiveStream>) })
    }
    fn install(&self, _: Option<PathBuf>) -> BoxFuture<'static, Result<(), String>> {
        Box::pin(async { Ok(()) })
    }
    fn unload(&self) -> BoxFuture<'static, Result<(), String>> {
        self.log.lock().unwrap().unloads += 1;
        Box::pin(async { Ok(()) })
    }
}

struct FakeStream {
    restart: Option<(usize, Arc<AtomicU64>)>,
    script: Script,
    log: Arc<Mutex<Log>>,
    samples: usize,
    tx: tokio::sync::mpsc::UnboundedSender<Event>,
    rx: tokio::sync::mpsc::UnboundedReceiver<Event>,
}
impl Drop for FakeStream {
    fn drop(&mut self) {
        self.log.lock().unwrap().dropped = true;
    }
}
impl LiveStream for FakeStream {
    fn send<'a>(&'a mut self, pcm: &'a [i16]) -> BoxFuture<'a, Result<(), String>> {
        let before = self.samples;
        self.samples += pcm.len();
        if let Some((after, instance)) = &self.restart
            && self.samples >= *after
            && before < *after
        {
            instance.store(2, Ordering::SeqCst);
        }
        let mut log = self.log.lock().unwrap();
        log.samples = self.samples;
        *log.per_stream.last_mut().unwrap() = self.samples;
        log.values.last_mut().unwrap().extend(pcm.iter().copied());
        drop(log);
        match &self.script {
            Script::Speak { .. } if self.samples / 1600 > before / 1600 => {
                let _ = self
                    .tx
                    .send(Event::Partial(format!("heard {}", self.samples / 1600)));
            }
            Script::FailAfter(limit) if self.samples >= *limit && before < *limit => {
                let _ = self
                    .tx
                    .send(Event::Failed("engine: the fake failed".into()));
            }
            _ => {}
        }
        Box::pin(async { Ok(()) })
    }
    fn finish(&mut self) -> BoxFuture<'_, Result<(), String>> {
        self.log.lock().unwrap().finished = true;
        if let Script::Speak { final_text } = &self.script {
            let _ = self.tx.send(Event::Final {
                text: final_text.clone(),
                model: Some("nemotron-3.5@rev1".into()),
            });
        }
        Box::pin(async { Ok(()) })
    }
    fn next(&mut self) -> BoxFuture<'_, Option<Event>> {
        Box::pin(async move { self.rx.recv().await })
    }
}

// ------------------------------------------------------------ fake microphone

/// Delivers 10 ms frames of 16 kHz audio every 10 ms until stopped, counting
/// what it delivered.
struct LiveTap {
    id: TapId,
    stop: Arc<AtomicBool>,
    value: f32,
    delivered: Arc<AtomicUsize>,
}
impl AudioTap for LiveTap {
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
        let stop = self.stop.clone();
        let value = self.value;
        let delivered = self.delivered.clone();
        std::thread::spawn(move || {
            while !stop.load(Ordering::Acquire) {
                sink.on_frames(
                    &[value; 160],
                    CaptureTimestamp::new(0, 0),
                    FrameFlags::empty(),
                );
                delivered.fetch_add(160, Ordering::AcqRel);
                std::thread::sleep(Duration::from_millis(10));
            }
        });
        Ok(self.format())
    }
    fn stop(&mut self) -> Result<(), TapError> {
        self.stop.store(true, Ordering::Release);
        Ok(())
    }
}

fn service(backend: &Arc<FakeBackend>) -> Arc<Service> {
    let service = Arc::new(Service::new(backend.clone()).unwrap());
    service.warm();
    service
}

fn streaming(service: &Arc<Service>, fallback: Option<&str>) -> CaptureProvider {
    CaptureProvider::Streaming {
        stream: Endpoint::new(service.clone()),
        fallback: fallback.map(|url| murmur::local_stt::LocalEndpoint::new(url).unwrap().into()),
    }
}

/// Samples in `ms` milliseconds of 16 kHz audio.
fn ms(ms: usize) -> usize {
    ms * 16
}

/// One dictation: hold until the mic has delivered `hold` samples, then
/// release (control 1) or Esc (2). Counting samples instead of wall time
/// keeps the tests independent of how fast the runner is.
async fn dictate(
    provider: CaptureProvider,
    progress: Progress,
    hold: usize,
    end: u8,
    seen_while_held: Arc<Mutex<Vec<String>>>,
) -> Result<String, String> {
    dictate_with(
        provider,
        progress,
        hold,
        end,
        seen_while_held,
        murmur::vocabulary::Profile::default(),
        false,
        0.25,
    )
    .await
}

/// [`dictate`] with a vocabulary profile and the mic's signal level; `expand`
/// returns the text after corrections and snippet expansion, as it would be
/// typed.
#[allow(clippy::too_many_arguments, reason = "test fixture knobs")]
async fn dictate_with(
    provider: CaptureProvider,
    progress: Progress,
    hold: usize,
    end: u8,
    seen_while_held: Arc<Mutex<Vec<String>>>,
    profile: murmur::vocabulary::Profile,
    expand: bool,
    value: f32,
) -> Result<String, String> {
    let control = Arc::new(AtomicU8::new(0));
    let delivered = Arc::new(AtomicUsize::new(0));
    let tap = LiveTap {
        id: TapId::mic("live"),
        stop: Arc::new(AtomicBool::new(false)),
        value,
        delivered: delivered.clone(),
    };
    let watcher = {
        let control = control.clone();
        let progress = progress.clone();
        tokio::spawn(async move {
            let started = tokio::time::Instant::now();
            while delivered.load(Ordering::Acquire) < hold {
                assert!(
                    started.elapsed() < Duration::from_secs(30),
                    "the fake mic stalled"
                );
                if let Some(text) = progress.live() {
                    let mut seen = seen_while_held.lock().unwrap();
                    if seen.last() != Some(&text) {
                        seen.push(text);
                    }
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            control.store(end, Ordering::Release);
        })
    };
    let input = InputPreparation::new(move || Ok(PreparedInput::new(Box::new(tap))));
    let changes = fotw_audio::device_change::DeviceChangeSignal::new();
    let receiving = Arc::new(AtomicBool::new(false));
    let startup = MicrophoneStartup::default();
    let result = if expand {
        murmur::capture::run_provider_with_preparation(
            input, provider, control, receiving, progress, startup, changes, profile,
        )
        .await
    } else {
        murmur::capture::run_provider_raw_with_preparation(
            input, provider, control, receiving, progress, startup, changes, profile,
        )
        .await
    };
    watcher.await.unwrap();
    result
}

/// A Whisper gateway that answers `text` and reports how many samples it got.
async fn batch_gateway(text: &'static str) -> (String, tokio::task::JoinHandle<usize>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let handle = tokio::spawn(async move {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut buf = vec![0; 65_536];
            loop {
                let n = socket.read(&mut buf).await.unwrap();
                request.extend_from_slice(&buf[..n]);
                let text = String::from_utf8_lossy(&request);
                if let Some(head) = text.find("\r\n\r\n") {
                    let length = text[..head]
                        .lines()
                        .find_map(|l| {
                            l.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .map(|v| v.trim().parse::<usize>().unwrap())
                        })
                        .unwrap_or(0);
                    if request.len() >= head + 4 + length {
                        break;
                    }
                }
                if n == 0 {
                    break;
                }
            }
            let head = String::from_utf8_lossy(&request);
            let (body, status) = if head.starts_with("GET /health") {
                (
                    serde_json::json!({"status":"ok","model":"fixture","language":"en","instance":"i1","vad":false}).to_string(),
                    "200 OK",
                )
            } else if head.starts_with("GET") {
                (serde_json::json!({"status":"ok"}).to_string(), "200 OK")
            } else {
                // The multipart WAV: its data chunk's length is the sample count × 2.
                let at = request.windows(4).position(|w| w == b"data").unwrap();
                let bytes = u32::from_le_bytes(request[at + 4..at + 8].try_into().unwrap());
                let response = serde_json::json!({"text": text}).to_string();
                let reply = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\nx-whisper-instance: i1\r\nx-whisper-model: fixture\r\ncontent-length: {}\r\n\r\n{}",
                    response.len(),
                    response
                );
                socket.write_all(reply.as_bytes()).await.unwrap();
                return bytes as usize / 2;
            };
            let reply = format!(
                "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{}",
                body.len(),
                body
            );
            socket.write_all(reply.as_bytes()).await.unwrap();
        }
    });
    (url, handle)
}

// ------------------------------------------------------------ tests

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn partials_show_while_held_and_only_the_final_is_returned() {
    let backend = FakeBackend::new(Script::Speak {
        final_text: "The whole  sentence,\nfinal.".into(),
    });
    let service = service(&backend);
    let progress = Progress::default();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let text = dictate(
        streaming(&service, None),
        progress.clone(),
        ms(1000),
        1,
        seen.clone(),
    )
    .await
    .expect("a final");
    assert_eq!(
        text, "The whole sentence, final.",
        "only the final, as one line"
    );
    let seen = seen.lock().unwrap().clone();
    assert!(
        seen.len() >= 2 && seen.iter().all(|t| t.starts_with("heard ")),
        "partials were shown while Control was held: {seen:?}"
    );
    assert_eq!(progress.stage(), Stage::Streaming { corrections: 0 });
    let log = backend.log();
    assert!(log.finished, "release flushes the stream");
    assert!(log.samples >= 16 * 500, "the audio streamed while held");
    assert_eq!(log.languages, vec![Some("en".to_owned())]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn escape_mid_stream_inserts_nothing_and_nothing_arrives_after() {
    let backend = FakeBackend::new(Script::Speak {
        final_text: "must never be typed".into(),
    });
    let service = service(&backend);
    let progress = Progress::default();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let result = dictate(
        streaming(&service, None),
        progress.clone(),
        ms(800),
        2,
        seen.clone(),
    )
    .await;
    assert_eq!(result, Err("Cancelled".into()));
    assert!(!seen.lock().unwrap().is_empty(), "it was streaming");
    {
        let log = backend.log();
        assert!(log.dropped, "the stream was cancelled in the helper");
        assert!(!log.finished, "and never flushed into a final");
    }
    // A partial or final the engine sends late reaches nothing.
    let before = progress.live();
    if let Some(late) = backend.late.lock().unwrap().as_ref() {
        let _ = late.send(Event::Partial("late partial".into()));
        let _ = late.send(Event::Final {
            text: "late final".into(),
            model: Some("nemotron-3.5@rev1".into()),
        });
    }
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(progress.live(), before, "no late partial after cancel");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_command_utterance_comes_back_whole_for_the_command_path() {
    let backend = FakeBackend::new(Script::Speak {
        final_text: "Murmur, use local.".into(),
    });
    let service = service(&backend);
    let raw = dictate(
        streaming(&service, None),
        Progress::default(),
        ms(300),
        1,
        Arc::default(),
    )
    .await
    .unwrap();
    // The worker applies a whole-utterance command instead of inserting it.
    assert_eq!(
        murmur::engine_command::parse(&raw),
        Some(murmur::engine_command::Command::Use(
            murmur::engine_command::Target::Local
        ))
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_engine_error_mid_stream_retries_the_kept_audio_on_the_batch_engine() {
    let backend = FakeBackend::new(Script::FailAfter(3_200));
    let service = service(&backend);
    let (url, gateway) = batch_gateway("Batch retry text.").await;
    let progress = Progress::default();
    let text = dictate(
        streaming(&service, Some(&url)),
        progress.clone(),
        ms(700),
        1,
        Arc::default(),
    )
    .await
    .expect("the batch engine transcribes the kept audio");
    assert_eq!(text, "Batch retry text.");
    let retried = gateway.await.unwrap();
    assert!(
        retried >= 16 * 650,
        "all the attempt's audio was kept, before and after the failure: {retried} samples"
    );
    assert_eq!(
        progress.stage(),
        Stage::StreamingRetried {
            engine: LocalEngine::Whisper
        }
    );
    assert!(
        Stage::StreamingRetried {
            engine: LocalEngine::Whisper
        }
        .completion_message("Paste sent")
        .contains("Streaming failed; retried on Local Whisper"),
        "the user is told"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_engine_error_without_a_batch_engine_reports_and_inserts_nothing() {
    let backend = FakeBackend::new(Script::FailAfter(3_200));
    let service = service(&backend);
    let error = dictate(
        streaming(&service, None),
        Progress::default(),
        ms(400),
        1,
        Arc::default(),
    )
    .await
    .unwrap_err();
    assert!(
        error.contains("Local streaming failed") && error.contains("the fake failed"),
        "{error}"
    );
    assert!(error.contains("no text inserted"), "{error}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_rapid_dictations_never_mix_stream_state() {
    let backend = FakeBackend::new(Script::Speak {
        final_text: "first".into(),
    });
    let service = service(&backend);
    let first = Progress::default();
    let result = dictate(
        streaming(&service, None),
        first.clone(),
        ms(1000),
        2,
        Arc::default(),
    )
    .await;
    assert_eq!(result, Err("Cancelled".into()));
    let first_late = backend.late.lock().unwrap().clone().unwrap();

    *backend.script.lock().unwrap() = Script::Speak {
        final_text: "second".into(),
    };
    let second = Progress::default();
    let seen = Arc::new(Mutex::new(Vec::new()));
    // The second attempt's microphone carries a different signal, so each
    // stream's audio can be told apart.
    let text = dictate_with(
        streaming(&service, None),
        second.clone(),
        ms(200),
        1,
        seen.clone(),
        murmur::vocabulary::Profile::default(),
        false,
        0.5,
    )
    .await
    .unwrap();
    // The first stream's late events go nowhere near the second attempt.
    let _ = first_late.send(Event::Partial("from the first".into()));
    assert_eq!(text, "second");
    assert!(
        seen.lock().unwrap().iter().all(|t| t != "from the first"),
        "the second attempt showed only its own partials"
    );
    assert_eq!(backend.log().opened, 2, "a fresh stream per dictation");
    // Each stream got only its own attempt's audio.
    let values = backend.log().values.clone();
    let first_signal = (0.25f32 * 32767.0) as i16;
    let second_signal = (0.5f32 * 32767.0) as i16;
    assert!(
        values[0].contains(&first_signal) && !values[0].contains(&second_signal),
        "first stream: {:?}",
        values[0]
    );
    assert!(
        values[1].contains(&second_signal) && !values[1].contains(&first_signal),
        "second stream: {:?}",
        values[1]
    );
    assert!(backend.log().per_stream[1] >= ms(200));
    assert_ne!(first.live(), second.live());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn streaming_not_ready_falls_back_to_the_batch_engine_before_recording() {
    let backend = FakeBackend::new(Script::Speak {
        final_text: "streamed".into(),
    });
    *backend.load.lock().unwrap() = Err(LoadError::Missing);
    let service = service(&backend);
    let (url, gateway) = batch_gateway("From the batch engine.").await;
    let progress = Progress::default();
    let text = dictate(
        streaming(&service, Some(&url)),
        progress.clone(),
        ms(300),
        1,
        Arc::default(),
    )
    .await
    .unwrap();
    assert_eq!(text, "From the batch engine.");
    assert!(gateway.await.unwrap() > 0);
    assert_eq!(backend.log().opened, 0, "no stream was opened");
    assert_eq!(
        progress.stage(),
        Stage::StreamingUnavailable {
            engine: LocalEngine::Whisper
        }
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_helper_restart_after_the_mic_opened_inserts_nothing_from_the_stream() {
    let backend = FakeBackend::new(Script::Speak {
        final_text: "from a stale helper".into(),
    });
    // The helper restarts while the stream is running, not before it opens.
    *backend.restart_after.lock().unwrap() = Some(3_200);
    let service = service(&backend);
    let error = dictate(
        streaming(&service, None),
        Progress::default(),
        ms(400),
        1,
        Arc::default(),
    )
    .await
    .unwrap_err();
    assert!(error.contains("restarted"), "{error}");
    assert_eq!(backend.log().opened, 1, "the stream had opened");
    assert!(backend.log().samples >= 3_200, "and was mid-utterance");
}

#[test]
fn local_streaming_is_saved_and_never_needs_cloud_credentials() {
    let settings: murmur::speech::Settings = serde_json::from_str(
        r#"{"provider":"local-streaming","local_endpoint":"http://127.0.0.1:17863"}"#,
    )
    .expect("local-streaming is a provider");
    let provider = settings
        .capture_provider(|| panic!("streaming must never load cloud credentials"))
        .unwrap();
    assert!(matches!(provider, CaptureProvider::Streaming { .. }));
    // Offline-all keeps streaming: it is local.
    let path = std::env::temp_dir().join(format!("murmur-streaming-{}.json", std::process::id()));
    settings.save(&path).unwrap();
    let selection = murmur::speech::Selection::load(Ok(path.clone()));
    let provider = selection
        .capture_provider_with_privacy(true, || panic!("no cloud"))
        .unwrap();
    assert!(matches!(provider, CaptureProvider::Streaming { .. }));
    let _ = std::fs::remove_file(path);
}

#[test]
fn murmur_use_streaming_is_a_command_that_switches_to_local_streaming() {
    use murmur::engine_command::{Command, Decision, Target, decide, parse};
    use murmur::speech::Provider;
    assert_eq!(
        parse("Murmur, use streaming."),
        Some(Command::Use(Target::Streaming))
    );
    assert_eq!(
        parse("Murmur switch to streaming"),
        Some(Command::Use(Target::Streaming))
    );
    assert_eq!(
        parse("Please use streaming"),
        None,
        "only the whole utterance"
    );
    let ok = Ok(());
    match decide(
        Command::Use(Target::Streaming),
        Some(&Provider::Deepgram),
        None,
        LocalEngine::Parakeet,
        true,
        &ok,
        &Err("no key".into()),
    ) {
        Decision::Switch { to, message } => {
            assert_eq!(to, Provider::LocalStreaming);
            assert!(message.contains("Local streaming"), "{message}");
        }
        other => panic!("streaming is local, allowed under Offline-all: {other:?}"),
    }
    assert!(matches!(
        decide(
            Command::Use(Target::Streaming),
            Some(&Provider::Deepgram),
            None,
            LocalEngine::Parakeet,
            false,
            &Err("Local streaming is still loading its model".into()),
            &ok,
        ),
        Decision::Refuse(_)
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_streaming_final_gets_vocabulary_corrections_before_snippets() {
    // #102 for streaming: the final is corrected like every local engine's
    // text, then snippets expand from the corrected text.
    let backend = FakeBackend::new(Script::Speak {
        final_text: "Insert snippet Fluid Audio.".into(),
    });
    let service = service(&backend);
    let profile = murmur::vocabulary::Profile {
        recognition_hints: vec!["FluidAudio".into()],
        snippets: vec![murmur::vocabulary::Snippet {
            name: "FluidAudio".into(),
            text: "Expanded only after correction".into(),
        }],
        ..Default::default()
    };
    let progress = Progress::default();
    let text = dictate_with(
        streaming(&service, None),
        progress.clone(),
        ms(300),
        1,
        Arc::default(),
        profile,
        true,
        0.25,
    )
    .await
    .unwrap();
    assert_eq!(text, "Expanded only after correction");
    assert_eq!(progress.stage(), Stage::Streaming { corrections: 1 });
    assert_eq!(
        progress.stage().label(),
        "Local streaming: 1 vocabulary correction available"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_switch_probe_runs_in_the_saved_language() {
    // "Murmur, use streaming" checks streaming can take the saved dictation
    // language (#108), not only that it is loaded.
    use murmur::engine_command::{Command, Decision, Target, decide};
    let backend = FakeBackend::new(Script::Speak {
        final_text: "unused".into(),
    });
    let service = service(&backend);
    let endpoint = Endpoint::new(service);
    endpoint
        .switch_probe("en")
        .await
        .expect("English is supported");
    let refused = endpoint.switch_probe("fr").await;
    assert!(
        refused
            .as_ref()
            .is_err_and(|e| e.contains("does not transcribe")),
        "{refused:?}"
    );
    match decide(
        Command::Use(Target::Streaming),
        Some(&murmur::speech::Provider::LocalParakeet),
        None,
        LocalEngine::Parakeet,
        false,
        &refused,
        &Ok(()),
    ) {
        Decision::Refuse(message) => assert!(
            message.contains("does not transcribe") && message.contains("staying on"),
            "{message}"
        ),
        other => panic!("the switch must be refused: {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unloading_streaming_releases_the_model_and_the_next_use_reloads_it() {
    let backend = FakeBackend::new(Script::Speak {
        final_text: "unused".into(),
    });
    let service = service(&backend);
    service.ready(Duration::from_secs(2)).await.unwrap();
    assert_eq!(backend.log().loads, 1);
    service.unload().await.unwrap();
    assert_eq!(
        backend.log().unloads,
        1,
        "the helper was asked to unload it"
    );
    assert!(service.state().is_none(), "no load is kept");
    service.ready(Duration::from_secs(2)).await.unwrap();
    assert_eq!(backend.log().loads, 2, "the next use loads it again");
}

#[test]
fn streaming_finalization_outlasts_its_own_retry_deadlines() {
    // After release a failed stream waits for its final (10 s), then the
    // batch engine's load (30 s) and transcription (180 s). The attempt must
    // not be abandoned as a generic "Finalization timed out" before those
    // report their own, clearer errors.
    let backend = FakeBackend::new(Script::Speak {
        final_text: "unused".into(),
    });
    let provider = streaming(&service(&backend), None);
    let limit = murmur::capture::finalization_limit(&provider);
    assert!(
        limit > Duration::from_secs(10 + 30 + 180),
        "streaming finalization limit {limit:?}"
    );
}
