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
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
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
}

struct FakeBackend {
    script: Mutex<Script>,
    load: Mutex<Result<(), LoadError>>,
    instance: AtomicU64,
    log: Arc<Mutex<Log>>,
    /// Lets a test push events into the newest stream after it has ended.
    late: Arc<Mutex<Option<tokio::sync::mpsc::UnboundedSender<Event>>>>,
}

impl FakeBackend {
    fn new(script: Script) -> Arc<Self> {
        Arc::new(Self {
            script: Mutex::new(script),
            load: Mutex::new(Ok(())),
            instance: AtomicU64::new(1),
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
        drop(log);
        let stream = FakeStream {
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
}

struct FakeStream {
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
        let mut log = self.log.lock().unwrap();
        log.samples = self.samples;
        *log.per_stream.last_mut().unwrap() = self.samples;
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

/// Delivers 10 ms frames of 16 kHz audio every 10 ms until stopped.
struct LiveTap {
    id: TapId,
    stop: Arc<AtomicBool>,
    value: f32,
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
        std::thread::spawn(move || {
            while !stop.load(Ordering::Acquire) {
                sink.on_frames(
                    &[value; 160],
                    CaptureTimestamp::new(0, 0),
                    FrameFlags::empty(),
                );
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
        fallback: fallback.map(|url| murmur::local_stt::LocalEndpoint::new(url).unwrap()),
    }
}

/// One dictation: hold for `hold`, then release (control 1) or Esc (2).
async fn dictate(
    provider: CaptureProvider,
    progress: Progress,
    hold: Duration,
    end: u8,
    seen_while_held: Arc<Mutex<Vec<String>>>,
) -> Result<String, String> {
    let control = Arc::new(AtomicU8::new(0));
    let tap = LiveTap {
        id: TapId::mic("live"),
        stop: Arc::new(AtomicBool::new(false)),
        value: 0.25,
    };
    let watcher = {
        let control = control.clone();
        let progress = progress.clone();
        tokio::spawn(async move {
            let started = tokio::time::Instant::now();
            while started.elapsed() < hold {
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
    let result = murmur::capture::run_provider_raw_with_preparation(
        InputPreparation::new(move || Ok(PreparedInput::new(Box::new(tap)))),
        provider,
        control,
        Arc::new(AtomicBool::new(false)),
        progress,
        MicrophoneStartup::default(),
        fotw_audio::device_change::DeviceChangeSignal::new(),
        murmur::vocabulary::Profile::default(),
    )
    .await;
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
        Duration::from_millis(600),
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
    assert_eq!(progress.stage(), Stage::Streaming);
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
        Duration::from_millis(400),
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
        Duration::from_millis(300),
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
        Duration::from_millis(700),
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
    assert_eq!(progress.stage(), Stage::StreamingRetried);
    assert!(
        Stage::StreamingRetried
            .completion_message("Paste sent")
            .contains("Streaming failed"),
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
        Duration::from_millis(400),
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
        Duration::from_millis(1000),
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
    let text = dictate(
        streaming(&service, None),
        second.clone(),
        Duration::from_millis(200),
        1,
        seen.clone(),
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
    // Each stream got only its own attempt's audio: the first ~1 s, the
    // second ~200 ms (plus mic start/stop slack), never the two together.
    let per_stream = backend.log().per_stream.clone();
    assert!(
        per_stream[0] >= 16 * 600 && per_stream[1] > 0 && per_stream[1] < per_stream[0],
        "per stream: {per_stream:?}"
    );
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
        Duration::from_millis(300),
        1,
        Arc::default(),
    )
    .await
    .unwrap();
    assert_eq!(text, "From the batch engine.");
    assert!(gateway.await.unwrap() > 0);
    assert_eq!(backend.log().opened, 0, "no stream was opened");
    assert_eq!(progress.stage(), Stage::StreamingUnavailable);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_helper_restart_after_the_mic_opened_inserts_nothing_from_the_stream() {
    let backend = FakeBackend::new(Script::Speak {
        final_text: "from a stale helper".into(),
    });
    let service = service(&backend);
    let control_backend = backend.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(150)).await;
        control_backend.instance.store(2, Ordering::SeqCst);
    });
    let error = dictate(
        streaming(&service, None),
        Progress::default(),
        Duration::from_millis(400),
        1,
        Arc::default(),
    )
    .await
    .unwrap_err();
    assert!(error.contains("restarted"), "{error}");
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
            false,
            &Err("Local streaming is still loading its model".into()),
            &ok,
        ),
        Decision::Refuse(_)
    ));
}
