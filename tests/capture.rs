use fotw_audio::{
    AudioTap, CaptureTimestamp, FrameFlags, FrameSink, SampleFormat, StreamFormat, TapError, TapId,
};
use fotw_stt::DeepgramEndpoint;
use futures_util::{SinkExt, StreamExt};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering},
};
use std::time::Duration;
use tokio_tungstenite::{accept_async, tungstenite::Message};
struct FakeTap {
    id: TapId,
    started: Arc<AtomicUsize>,
    stopped: Arc<AtomicUsize>,
    control: Arc<AtomicU8>,
    rate: u32,
}
impl AudioTap for FakeTap {
    fn id(&self) -> &TapId {
        &self.id
    }
    fn format(&self) -> StreamFormat {
        StreamFormat::new(self.rate, 1, SampleFormat::F32)
    }
    fn format_is_authoritative(&self) -> bool {
        true
    }
    fn start(&mut self, mut sink: Box<dyn FrameSink>) -> Result<StreamFormat, TapError> {
        self.started.fetch_add(1, Ordering::SeqCst);
        sink.on_frames(
            &vec![0.25; 4800],
            CaptureTimestamp::new(0, 0),
            FrameFlags::empty(),
        );
        self.control.store(1, Ordering::Release);
        Ok(self.format())
    }
    fn stop(&mut self) -> Result<(), TapError> {
        self.stopped.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}
type Fixture = (
    Box<dyn AudioTap>,
    Arc<AtomicU8>,
    Arc<AtomicUsize>,
    Arc<AtomicUsize>,
);
fn fixture(rate: u32, initial: u8) -> Fixture {
    let control = Arc::new(AtomicU8::new(initial));
    let started = Arc::new(AtomicUsize::new(0));
    let stopped = Arc::new(AtomicUsize::new(0));
    let tap = FakeTap {
        id: TapId::mic("fixture"),
        started: started.clone(),
        stopped: stopped.clone(),
        control: control.clone(),
        rate,
    };
    (Box::new(tap), control, started, stopped)
}
#[tokio::test]
async fn cancelled_start_never_opens_microphone_or_provider() {
    let (tap, control, started, _) = fixture(48000, 2);
    let receiving = Arc::new(AtomicBool::new(false));
    assert!(
        murmur::capture::run_observed(
            tap,
            "test-key".into(),
            control,
            DeepgramEndpoint::production(),
            receiving.clone()
        )
        .await
        .is_err()
    );
    assert_eq!(started.load(Ordering::SeqCst), 0);
    assert!(!receiving.load(Ordering::Acquire));
}
#[tokio::test]
async fn invalid_actual_format_closes_microphone_before_network() {
    let (tap, control, started, stopped) = fixture(0, 0);
    assert!(
        murmur::capture::run(tap, "test-key".into(), control)
            .await
            .unwrap_err()
            .contains("format")
    );
    assert_eq!(started.load(Ordering::SeqCst), 1);
    assert!(stopped.load(Ordering::SeqCst) > 0);
}
#[tokio::test]
async fn release_drains_microphone_through_resampler_and_closes_capture() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let mut ws = accept_async(socket).await.unwrap();
        let mut audio = false;
        while let Some(Ok(msg)) = ws.next().await {
            match msg {
                Message::Binary(bytes) => {
                    audio = true;
                    assert_eq!(bytes.len() % 2, 0);
                    assert!(bytes.iter().any(|b| *b != 0));
                }
                Message::Text(t) if t.contains("CloseStream") => {
                    assert!(audio);
                    ws.send(Message::text(r#"{"type":"Results","is_final":true,"speech_final":true,"channel":{"alternatives":[{"transcript":"Fixture speech."}]}}"#)).await.unwrap();
                    ws.send(Message::text(r#"{"type":"Metadata","duration":0.3}"#))
                        .await
                        .unwrap();
                    break;
                }
                _ => {}
            }
        }
    });
    let (tap, control, started, stopped) = fixture(48000, 0);
    let receiving = Arc::new(AtomicBool::new(false));
    let text = tokio::time::timeout(
        Duration::from_secs(3),
        murmur::capture::run_observed(
            tap,
            "test-key".into(),
            control,
            DeepgramEndpoint::loopback(port),
            receiving.clone(),
        ),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(text, "Fixture speech.");
    assert!(receiving.load(Ordering::Acquire));
    assert_eq!(started.load(Ordering::SeqCst), 1);
    assert!(stopped.load(Ordering::SeqCst) > 0);
    server.await.unwrap();
}
#[tokio::test]
async fn local_release_sends_wav_without_cloud_key_and_stops_microphone() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = murmur::local_stt::LocalEndpoint::new(&format!(
        "http://{}",
        listener.local_addr().unwrap()
    ))
    .unwrap();
    let server = tokio::spawn(async move {
        let (mut probe, _) = listener.accept().await.unwrap();
        let mut request = [0; 4096];
        let n = probe.read(&mut request).await.unwrap();
        assert!(request[..n].starts_with(b"GET /health "));
        let health = r#"{"status":"ok"}"#;
        probe
            .write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{health}",
                    health.len()
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        drop(probe);
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut bytes = Vec::new();
        loop {
            let mut buf = [0; 8192];
            let n = socket.read(&mut buf).await.unwrap();
            assert!(n > 0);
            bytes.extend_from_slice(&buf[..n]);
            if let Some(end) = bytes.windows(4).position(|v| v == b"\r\n\r\n") {
                let header = String::from_utf8_lossy(&bytes[..end]).to_lowercase();
                let length: usize = header
                    .lines()
                    .find_map(|l| l.strip_prefix("content-length: "))
                    .unwrap()
                    .parse()
                    .unwrap();
                if bytes.len() >= end + 4 + length {
                    break;
                }
            }
        }
        assert!(bytes.windows(4).any(|v| v == b"RIFF"));
        let body = r#"{"text":"Local fixture."}"#;
        socket
            .write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .as_bytes(),
            )
            .await
            .unwrap();
    });
    let (tap, control, started, stopped) = fixture(48000, 0);
    let result = murmur::capture::run_provider_observed(
        tap,
        murmur::capture::CaptureProvider::Local(endpoint),
        control,
        Arc::new(AtomicBool::new(false)),
    )
    .await
    .unwrap();
    assert_eq!(result, "Local fixture.");
    assert_eq!(started.load(Ordering::SeqCst), 1);
    assert!(stopped.load(Ordering::SeqCst) > 0);
    server.await.unwrap();
}

#[test]
fn stalled_dns_lookup_does_not_hold_back_a_failed_result() {
    let started = std::time::Instant::now();
    let result = murmur::capture::block_on_detached(async {
        let lookup = tokio::task::spawn_blocking(|| std::thread::sleep(Duration::from_secs(3)));
        tokio::time::timeout(Duration::from_millis(50), lookup)
            .await
            .map_err(|_| "Provider connection timed out")
    });
    assert_eq!(
        result.unwrap().unwrap_err(),
        "Provider connection timed out"
    );
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "runtime shutdown held the UI completion hostage"
    );
}

#[tokio::test]
async fn unavailable_local_engine_is_reported_before_microphone_capture() {
    let (tap, control, started, _) = fixture(48000, 0);
    let result = murmur::capture::run_provider_observed(
        tap,
        murmur::capture::CaptureProvider::Local(
            murmur::local_stt::LocalEndpoint::new("http://127.0.0.1:1").unwrap(),
        ),
        control,
        Arc::new(AtomicBool::new(false)),
    )
    .await;
    assert!(result.is_err());
    assert_eq!(
        started.load(Ordering::SeqCst),
        0,
        "readiness failure must precede capture, not waste the utterance"
    );
}

/// A microphone whose `AudioDeviceStart` never returns until the gate opens,
/// the way CoreAudio wedges when its IO thread never reaches the run state.
struct WedgedTap {
    id: TapId,
    gate: std::sync::Mutex<std::sync::mpsc::Receiver<()>>,
    stopped: Arc<AtomicUsize>,
}
impl AudioTap for WedgedTap {
    fn id(&self) -> &TapId {
        &self.id
    }
    fn format(&self) -> StreamFormat {
        StreamFormat::new(48000, 1, SampleFormat::F32)
    }
    fn format_is_authoritative(&self) -> bool {
        true
    }
    fn start(&mut self, _: Box<dyn FrameSink>) -> Result<StreamFormat, TapError> {
        let _ = self
            .gate
            .lock()
            .unwrap()
            .recv_timeout(Duration::from_secs(30));
        Ok(self.format())
    }
    fn stop(&mut self) -> Result<(), TapError> {
        self.stopped.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}
fn wedged() -> (
    Box<dyn AudioTap>,
    std::sync::mpsc::Sender<()>,
    Arc<AtomicUsize>,
) {
    let (open, gate) = std::sync::mpsc::channel();
    let stopped = Arc::new(AtomicUsize::new(0));
    let tap = WedgedTap {
        id: TapId::mic("wedged"),
        gate: std::sync::Mutex::new(gate),
        stopped: stopped.clone(),
    };
    (Box::new(tap), open, stopped)
}
#[tokio::test]
async fn wedged_microphone_start_fails_instead_of_loading_forever() {
    let (tap, _open, _) = wedged();
    let control = Arc::new(AtomicU8::new(0));
    let started = std::time::Instant::now();
    let result = tokio::time::timeout(
        Duration::from_secs(10),
        murmur::capture::run(tap, "test-key".into(), control),
    )
    .await
    .expect("a wedged microphone start must not hang the attempt");
    assert!(result.unwrap_err().contains("Microphone did not start"));
    assert!(started.elapsed() <= murmur::capture::MIC_START_DEADLINE + Duration::from_secs(1));
}
#[tokio::test]
async fn escape_cancels_while_microphone_is_starting() {
    let (tap, _open, _) = wedged();
    let control = Arc::new(AtomicU8::new(0));
    let cancel = control.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        cancel.store(2, Ordering::Release);
    });
    let result = tokio::time::timeout(
        Duration::from_secs(1),
        murmur::capture::run(tap, "test-key".into(), control),
    )
    .await
    .expect("Esc must not wait for the microphone to start");
    assert_eq!(result.unwrap_err(), "Cancelled");
}
#[tokio::test]
async fn microphone_that_starts_after_giving_up_is_stopped() {
    let (tap, open, stopped) = wedged();
    let control = Arc::new(AtomicU8::new(0));
    let cancel = control.clone();
    let attempt = tokio::spawn(murmur::capture::run(tap, "test-key".into(), control));
    tokio::time::sleep(Duration::from_millis(100)).await;
    cancel.store(2, Ordering::Release);
    assert!(attempt.await.unwrap().is_err());
    open.send(()).unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while stopped.load(Ordering::SeqCst) == 0 && std::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(
        stopped.load(Ordering::SeqCst),
        1,
        "a late start must not leave the mic open"
    );
}

#[tokio::test]
async fn repeated_attempts_do_not_start_more_workers_until_abandoned_start_is_cleaned_up() {
    use murmur::capture::{CaptureProvider, MicrophoneStartup, run_provider_with_startup};
    let startup = MicrophoneStartup::default();
    let provider = || CaptureProvider::Deepgram {
        key: "fixture".into(),
        endpoint: DeepgramEndpoint::loopback(1),
    };
    let (tap, open, stopped) = wedged();
    let control = Arc::new(AtomicU8::new(0));
    let first = tokio::spawn(run_provider_with_startup(
        tap,
        provider(),
        control.clone(),
        Arc::new(AtomicBool::new(false)),
        Default::default(),
        startup.clone(),
    ));
    tokio::time::sleep(Duration::from_millis(30)).await;
    control.store(2, Ordering::Release);
    assert_eq!(first.await.unwrap().unwrap_err(), "Cancelled");
    for _ in 0..3 {
        let (tap, control, started, _) = fixture(0, 0);
        let result = run_provider_with_startup(
            tap,
            provider(),
            control,
            Arc::new(AtomicBool::new(false)),
            Default::default(),
            startup.clone(),
        )
        .await;
        assert!(result.unwrap_err().contains("still pending"));
        assert_eq!(started.load(Ordering::SeqCst), 0);
    }
    open.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        while stopped.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let (tap, control, started, _) = fixture(0, 0);
    let result = run_provider_with_startup(
        tap,
        provider(),
        control,
        Arc::new(AtomicBool::new(false)),
        Default::default(),
        startup,
    )
    .await;
    assert!(result.unwrap_err().contains("format"));
    assert_eq!(started.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn release_while_microphone_is_starting_does_not_wait_for_the_device() {
    let (tap, _open, _) = wedged();
    let control = Arc::new(AtomicU8::new(0));
    let released = control.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(30)).await;
        released.store(1, Ordering::Release);
    });
    let result = tokio::time::timeout(
        Duration::from_secs(1),
        murmur::capture::run(tap, "fixture".into(), control),
    )
    .await
    .unwrap();
    assert!(result.unwrap_err().contains("ended before microphone"));
}

#[derive(Clone, Copy)]
enum CaptureFault {
    None,
    Silent,
    Discontinuity,
    NonFinite,
    DuringStop,
    RouteAfterStop,
}
struct FaultTap {
    id: TapId,
    fault: CaptureFault,
    control: Arc<AtomicU8>,
    stopped: Arc<AtomicUsize>,
    sink: Option<Box<dyn FrameSink>>,
    changes: Arc<fotw_audio::device_change::DeviceChangeSignal>,
    event: Option<fotw_audio::device_change::DeviceChangeKind>,
}
impl AudioTap for FaultTap {
    fn id(&self) -> &TapId {
        &self.id
    }
    fn format(&self) -> StreamFormat {
        StreamFormat::new(48000, 1, SampleFormat::F32)
    }
    fn format_is_authoritative(&self) -> bool {
        true
    }
    fn start(&mut self, mut sink: Box<dyn FrameSink>) -> Result<StreamFormat, TapError> {
        let mut pcm = vec![0.25; 4800];
        if matches!(self.fault, CaptureFault::Silent) {
            pcm.fill(0.0);
        }
        if matches!(self.fault, CaptureFault::NonFinite) {
            pcm[100] = f32::NAN;
        }
        let flags = if matches!(self.fault, CaptureFault::Discontinuity) {
            FrameFlags::DISCONTINUITY
        } else if matches!(self.fault, CaptureFault::Silent) {
            FrameFlags::SILENT
        } else {
            FrameFlags::empty()
        };
        sink.on_frames(&pcm, CaptureTimestamp::new(0, 0), flags);
        self.sink = Some(sink);
        if let Some(event) = self.event {
            self.changes.raise(event);
        }
        self.control.store(1, Ordering::Release);
        Ok(self.format())
    }
    fn stop(&mut self) -> Result<(), TapError> {
        self.stopped.fetch_add(1, Ordering::SeqCst);
        if let Some(mut sink) = self.sink.take()
            && matches!(self.fault, CaptureFault::DuringStop)
        {
            sink.on_error(TapError::DeviceInvalidated(self.id.clone()));
        }
        Ok(())
    }
}
async fn fault_capture(fault: CaptureFault) -> Result<String, String> {
    event_capture(
        fault,
        None,
        fotw_audio::device_change::DeviceChangeSignal::new(),
    )
    .await
}
async fn event_capture(
    fault: CaptureFault,
    event: Option<fotw_audio::device_change::DeviceChangeKind>,
    changes: Arc<fotw_audio::device_change::DeviceChangeSignal>,
) -> Result<String, String> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server_changes = changes.clone();
    let server = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let mut ws = accept_async(socket).await.unwrap();
        while let Some(Ok(msg)) = ws.next().await {
            if let Message::Text(t) = msg
                && t.contains("CloseStream")
            {
                let _=ws.send(Message::text(r#"{"type":"Results","is_final":true,"speech_final":true,"channel":{"alternatives":[{"transcript":"Fixture speech."}]}}"#)).await;
                if matches!(fault, CaptureFault::RouteAfterStop) {
                    server_changes.raise(fotw_audio::device_change::DeviceChangeKind::DefaultInput);
                }
                let _ = ws
                    .send(Message::text(r#"{"type":"Metadata","duration":0.3}"#))
                    .await;
                break;
            }
        }
    });
    let control = Arc::new(AtomicU8::new(0));
    let stopped = Arc::new(AtomicUsize::new(0));
    let tap = FaultTap {
        id: TapId::mic("fault-fixture"),
        fault,
        control: control.clone(),
        stopped: stopped.clone(),
        sink: None,
        changes: changes.clone(),
        event,
    };
    let result = tokio::time::timeout(
        Duration::from_secs(3),
        murmur::capture::run_provider_with_events(
            Box::new(tap),
            murmur::capture::CaptureProvider::Deepgram {
                key: "fixture".into(),
                endpoint: DeepgramEndpoint::loopback(port),
            },
            control,
            Arc::new(AtomicBool::new(false)),
            Default::default(),
            Default::default(),
            changes,
        ),
    )
    .await;
    server.abort();
    let _ = server.await;
    assert!(
        stopped.load(Ordering::SeqCst) > 0,
        "failed capture must close its tap"
    );
    result.expect("capture failure must not hang")
}
#[tokio::test]
async fn capture_fault_discontinuity_rejects_partial_speech_and_next_attempt_works() {
    assert!(
        fault_capture(CaptureFault::Discontinuity)
            .await
            .unwrap_err()
            .contains("interrupted"),
        "discontinuous speech must not be inserted as complete"
    );
    assert_eq!(
        fault_capture(CaptureFault::None).await.unwrap(),
        "Fixture speech."
    );
}
#[tokio::test]
async fn capture_fault_nonfinite_samples_rejects_partial_speech() {
    assert!(
        fault_capture(CaptureFault::NonFinite)
            .await
            .unwrap_err()
            .contains("invalid samples"),
        "invalid samples must not become a successful transcript"
    );
}
#[tokio::test]
async fn capture_fault_during_stop_wins_over_fast_provider_completion() {
    assert!(
        fault_capture(CaptureFault::DuringStop)
            .await
            .unwrap_err()
            .contains("disconnected"),
        "microphone failure must win over a ready provider result"
    );
}

#[tokio::test]
async fn capture_silent_buffers_are_valid_delivery_not_device_loss() {
    assert_eq!(
        fault_capture(CaptureFault::Silent).await.unwrap(),
        "Fixture speech."
    );
}

#[tokio::test]
async fn device_event_input_switch_cancels_only_its_attempt() {
    use fotw_audio::device_change::{DeviceChangeKind, DeviceChangeSignal};
    let old = DeviceChangeSignal::new();
    let result = event_capture(
        CaptureFault::None,
        Some(DeviceChangeKind::DefaultInput),
        old.clone(),
    )
    .await;
    assert!(result.unwrap_err().contains("Microphone route changed"));
    // A delayed notification still belongs to the old capture, never the next one.
    old.raise(DeviceChangeKind::DefaultInput);
    assert_eq!(
        event_capture(CaptureFault::None, None, DeviceChangeSignal::new())
            .await
            .unwrap(),
        "Fixture speech."
    );
}
#[tokio::test]
async fn device_event_output_switch_does_not_cancel_microphone_dictation() {
    use fotw_audio::device_change::{DeviceChangeKind, DeviceChangeSignal};
    assert_eq!(
        event_capture(
            CaptureFault::None,
            Some(DeviceChangeKind::DefaultOutput),
            DeviceChangeSignal::new()
        )
        .await
        .unwrap(),
        "Fixture speech."
    );
}

#[tokio::test]
async fn device_event_after_capture_stop_does_not_discard_complete_speech() {
    assert_eq!(
        fault_capture(CaptureFault::RouteAfterStop).await.unwrap(),
        "Fixture speech."
    );
}

#[tokio::test]
async fn device_event_cancels_pending_start_without_waiting_for_os() {
    use fotw_audio::device_change::{DeviceChangeKind, DeviceChangeSignal};
    let (tap, open, stopped) = wedged();
    let changes = DeviceChangeSignal::new();
    let notify = changes.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(30)).await;
        notify.raise(DeviceChangeKind::DefaultInput);
    });
    let result = tokio::time::timeout(
        Duration::from_secs(1),
        murmur::capture::run_provider_with_events(
            tap,
            murmur::capture::CaptureProvider::Deepgram {
                key: "fixture".into(),
                endpoint: DeepgramEndpoint::loopback(1),
            },
            Arc::new(AtomicU8::new(0)),
            Arc::new(AtomicBool::new(false)),
            Default::default(),
            Default::default(),
            changes,
        ),
    )
    .await
    .expect("route change must not wait for OS startup");
    assert!(result.unwrap_err().contains("Microphone route changed"));
    open.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(1), async {
        while stopped.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
#[allow(clippy::result_large_err, reason = "tungstenite callback error type")]
async fn captured_utterance_uses_one_profile_and_expands_only_the_winning_final() {
    use murmur::vocabulary::{Profile, Snippet};
    use tokio_tungstenite::{
        accept_hdr_async,
        tungstenite::handshake::server::{Request, Response},
    };
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("vocabulary.json");
    let profile = Profile {
        version: 1,
        recognition_hints: vec!["Project Acorn".into()],
        snippets: vec![Snippet {
            name: "signature".into(),
            text: "Private fixture body\nExample".into(),
        }],
    };
    profile.save(&path).unwrap();
    let snapshot = Profile::load(&path).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = DeepgramEndpoint::loopback(listener.local_addr().unwrap().port());
    let changed = path.clone();
    let server = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let mut ws = accept_hdr_async(socket, |req: &Request, res: Response| {
            let uri = req.uri().to_string();
            assert!(uri.contains("keyterm=Project%20Acorn"));
            assert!(!uri.contains("Private"));
            Ok(res)
        })
        .await
        .unwrap();
        let mut updated = profile;
        updated.snippets[0].text = "New profile value".into();
        updated.save(&changed).unwrap();
        while let Some(Ok(message)) = ws.next().await {
            if matches!(message,Message::Text(ref text) if text.contains("CloseStream")) {
                ws.send(Message::text(r#"{"type":"Results","is_final":true,"speech_final":true,"channel":{"alternatives":[{"transcript":"Insert snippet signature."}]}}"#)).await.unwrap();
                ws.send(Message::text(r#"{"type":"Metadata"}"#))
                    .await
                    .unwrap();
                break;
            }
        }
    });
    let (tap, control, started, stopped) = fixture(48000, 0);
    let output = murmur::capture::run_provider_with_vocabulary(
        tap,
        murmur::capture::CaptureProvider::Deepgram {
            key: "fixture".into(),
            endpoint,
        },
        control,
        Arc::new(AtomicBool::new(false)),
        murmur::fallback::Progress::default(),
        murmur::capture::MicrophoneStartup::default(),
        fotw_audio::device_change::DeviceChangeSignal::new(),
        snapshot,
    )
    .await
    .unwrap();
    server.await.unwrap();
    assert_eq!(output, "Private fixture body\nExample");
    assert_eq!(
        Profile::load(&path)
            .unwrap()
            .expand("insert snippet signature"),
        "New profile value"
    );
    assert_eq!(started.load(Ordering::SeqCst), 1);
    assert!(stopped.load(Ordering::SeqCst) > 0);
}

#[tokio::test]
async fn invalid_profile_is_refused_before_microphone_start() {
    let (tap, control, started, _) = fixture(48000, 0);
    let profile = murmur::vocabulary::Profile {
        version: 2,
        ..Default::default()
    };
    let result = murmur::capture::run_provider_with_vocabulary(
        tap,
        murmur::capture::CaptureProvider::Deepgram {
            key: "fixture".into(),
            endpoint: DeepgramEndpoint::loopback(1),
        },
        control,
        Arc::new(AtomicBool::new(false)),
        murmur::fallback::Progress::default(),
        murmur::capture::MicrophoneStartup::default(),
        fotw_audio::device_change::DeviceChangeSignal::new(),
        profile,
    )
    .await;
    assert!(result.unwrap_err().contains("schema version"));
    assert_eq!(started.load(Ordering::SeqCst), 0);
}
