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
#[test]
fn stalled_dns_lookup_does_not_hold_back_a_failed_result() {
    // Offline, getaddrinfo can block for tens of seconds after the connect
    // deadline has already failed the attempt.
    let started = std::time::Instant::now();
    let result = murmur::capture::block_on_detached(async {
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let lookup = tokio::task::spawn_blocking(move || {
            let _ = started_tx.send(());
            std::thread::sleep(Duration::from_secs(5));
        });
        started_rx.await.expect("blocking lookup did not start");
        tokio::time::timeout(Duration::from_millis(50), lookup)
            .await
            .map_err(|_| "Provider connection timed out")
    });
    assert_eq!(
        result.unwrap().unwrap_err(),
        "Provider connection timed out"
    );
    assert!(started.elapsed() < Duration::from_secs(2));
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

#[derive(Clone, Copy)]
enum CaptureFault {
    None,
    Silent,
    Discontinuity,
    NonFinite,
    FormatChanged,
    DuringStop,
    RouteAfterStop,
    RouteAtSuccessfulStop,
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
        if matches!(self.fault, CaptureFault::FormatChanged) {
            sink.on_error(TapError::StreamFormatChanged);
        }
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
        if matches!(self.fault, CaptureFault::RouteAtSuccessfulStop) {
            self.changes
                .raise(fotw_audio::device_change::DeviceChangeKind::DefaultInput);
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
async fn capture_format_change_requires_a_new_dictation_instead_of_partial_text() {
    let error = fault_capture(CaptureFault::FormatChanged)
        .await
        .unwrap_err();
    assert!(error.contains("format changed"), "{error}");
    assert!(error.contains("new dictation"), "{error}");
    assert!(error.contains("no text inserted"), "{error}");
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
async fn device_event_at_successful_stop_does_not_discard_complete_speech() {
    assert_eq!(
        fault_capture(CaptureFault::RouteAtSuccessfulStop)
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

struct FixtureWatcher(Arc<AtomicUsize>);
impl fotw_audio::DeviceWatch for FixtureWatcher {}
impl Drop for FixtureWatcher {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn escape_during_input_preparation_returns_without_starting_a_late_microphone() {
    let blocked = blocked_preparation();
    let control = Arc::new(AtomicU8::new(0));
    let cancel = control.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        cancel.store(2, Ordering::Release);
    });
    let began = std::time::Instant::now();
    let result = prepared_fixture(blocked.input, control, Default::default()).await;
    let elapsed = began.elapsed();
    blocked.release.send(()).unwrap();
    assert!(result.unwrap_err().contains("Cancelled"));
    assert!(
        elapsed < Duration::from_millis(400),
        "Escape waited for native preparation: {elapsed:?}"
    );
    wait_for_counter(&blocked.watcher_drops).await;
    assert_eq!(
        blocked.starts.load(Ordering::SeqCst),
        0,
        "cancelled preparation must never start capture"
    );
    assert_eq!(
        blocked.stops.load(Ordering::SeqCst),
        1,
        "late owned resources are closed"
    );
}

struct BlockingPreparation {
    input: murmur::capture::InputPreparation,
    release: std::sync::mpsc::Sender<()>,
    entered: Arc<AtomicBool>,
    starts: Arc<AtomicUsize>,
    stops: Arc<AtomicUsize>,
    watcher_drops: Arc<AtomicUsize>,
}
fn blocked_preparation() -> BlockingPreparation {
    let (tap, _, starts, stops) = fixture(0, 0);
    let (release, gate) = std::sync::mpsc::channel();
    let entered = Arc::new(AtomicBool::new(false));
    let entered_worker = entered.clone();
    let watcher_drops = Arc::new(AtomicUsize::new(0));
    let drops = watcher_drops.clone();
    BlockingPreparation {
        input: murmur::capture::InputPreparation::new(move || {
            let watcher = FixtureWatcher(drops);
            entered_worker.store(true, Ordering::Release);
            let _ = gate.recv_timeout(Duration::from_secs(10));
            Ok(murmur::capture::PreparedInput::watched(
                tap,
                Box::new(watcher),
            ))
        }),
        release,
        entered,
        starts,
        stops,
        watcher_drops,
    }
}
async fn prepared_fixture(
    input: murmur::capture::InputPreparation,
    control: Arc<AtomicU8>,
    startup: murmur::capture::MicrophoneStartup,
) -> Result<String, String> {
    murmur::capture::run_provider_with_preparation(
        input,
        murmur::capture::CaptureProvider::Deepgram {
            key: "fixture".into(),
            endpoint: DeepgramEndpoint::loopback(1),
        },
        control,
        Arc::new(AtomicBool::new(false)),
        Default::default(),
        startup,
        fotw_audio::device_change::DeviceChangeSignal::new(),
        Default::default(),
    )
    .await
}
async fn wait_for_counter(counter: &AtomicUsize) {
    tokio::time::timeout(Duration::from_secs(1), async {
        while counter.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
}
#[tokio::test]
async fn release_during_preparation_and_late_deadline_both_prevent_capture() {
    for release_key in [true, false] {
        let blocked = blocked_preparation();
        let control = Arc::new(AtomicU8::new(0));
        if release_key {
            let end = control.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(50)).await;
                end.store(1, Ordering::Release);
            });
        }
        let began = std::time::Instant::now();
        let result = prepared_fixture(blocked.input, control, Default::default()).await;
        let elapsed = began.elapsed();
        blocked.release.send(()).unwrap();
        let error = result.unwrap_err();
        if release_key {
            assert!(error.contains("before microphone was ready"));
            assert!(elapsed < Duration::from_millis(400));
        } else {
            assert!(error.contains("Microphone did not start"), "{error}");
            assert!(elapsed <= murmur::capture::MIC_START_DEADLINE + Duration::from_secs(1));
        }
        wait_for_counter(&blocked.watcher_drops).await;
        assert_eq!(blocked.starts.load(Ordering::SeqCst), 0);
        assert_eq!(blocked.stops.load(Ordering::SeqCst), 1);
        assert_eq!(blocked.watcher_drops.load(Ordering::SeqCst), 1);
    }
}
#[tokio::test]
async fn abandoned_preparation_holds_the_lease_and_recovers_without_more_open_workers() {
    let blocked = blocked_preparation();
    let startup = murmur::capture::MicrophoneStartup::default();
    let control = Arc::new(AtomicU8::new(0));
    let first = tokio::spawn(prepared_fixture(
        blocked.input,
        control.clone(),
        startup.clone(),
    ));
    tokio::time::timeout(Duration::from_secs(1), async {
        while !blocked.entered.load(Ordering::Acquire) {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    control.store(2, Ordering::Release);
    assert!(first.await.unwrap().unwrap_err().contains("Cancelled"));
    let attempts = Arc::new(AtomicUsize::new(0));
    for _ in 0..3 {
        let count = attempts.clone();
        let input = murmur::capture::InputPreparation::new(move || {
            count.fetch_add(1, Ordering::SeqCst);
            Err("fixture preparation finished".into())
        });
        assert!(
            prepared_fixture(input, Arc::new(AtomicU8::new(0)), startup.clone())
                .await
                .unwrap_err()
                .contains("still pending")
        );
    }
    assert_eq!(
        attempts.load(Ordering::SeqCst),
        0,
        "repeated attempts must not watch/open more devices"
    );
    blocked.release.send(()).unwrap();
    wait_for_counter(&blocked.watcher_drops).await;
    // The watcher's Drop runs just before lease release. Retry that tiny cleanup
    // boundary rather than assuming its counter proves the whole guard is gone.
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            let count = attempts.clone();
            let input = murmur::capture::InputPreparation::new(move || {
                count.fetch_add(1, Ordering::SeqCst);
                Err("fixture preparation finished".into())
            });
            let error = prepared_fixture(input, Arc::new(AtomicU8::new(0)), startup.clone())
                .await
                .unwrap_err();
            if error == "fixture preparation finished" {
                break;
            }
            assert!(error.contains("still pending"));
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(attempts.load(Ordering::SeqCst), 1);
    assert_eq!(blocked.starts.load(Ordering::SeqCst), 0);
    assert_eq!(blocked.stops.load(Ordering::SeqCst), 1);
}
#[tokio::test]
async fn invalid_profile_and_unready_local_engine_do_not_prepare_native_input() {
    for invalid_profile in [true, false] {
        let calls = Arc::new(AtomicUsize::new(0));
        let observed = calls.clone();
        let input = murmur::capture::InputPreparation::new(move || {
            observed.fetch_add(1, Ordering::SeqCst);
            Err("must not prepare".into())
        });
        let profile = murmur::vocabulary::Profile {
            version: if invalid_profile { 999 } else { 1 },
            ..Default::default()
        };
        let error = murmur::capture::run_provider_with_preparation(
            input,
            murmur::capture::CaptureProvider::Local(
                murmur::local_stt::LocalEndpoint::new("http://127.0.0.1:1").unwrap(),
            ),
            Arc::new(AtomicU8::new(0)),
            Arc::new(AtomicBool::new(false)),
            Default::default(),
            Default::default(),
            fotw_audio::device_change::DeviceChangeSignal::new(),
            profile,
        )
        .await
        .unwrap_err();
        assert_ne!(error, "must not prepare");
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }
}

struct CleanupGate {
    gate: Option<std::sync::mpsc::Receiver<()>>,
    entered: Arc<AtomicBool>,
}
impl CleanupGate {
    fn wait(&mut self) {
        if let Some(gate) = self.gate.take() {
            self.entered.store(true, Ordering::Release);
            let _ = gate.recv_timeout(Duration::from_secs(2));
        }
    }
}
struct CleanupGateWatch(CleanupGate);
impl fotw_audio::DeviceWatch for CleanupGateWatch {}
impl Drop for CleanupGateWatch {
    fn drop(&mut self) {
        self.0.wait();
    }
}
struct StopGateTap {
    tap: Box<dyn AudioTap>,
    gate: CleanupGate,
}
impl AudioTap for StopGateTap {
    fn id(&self) -> &TapId {
        self.tap.id()
    }
    fn format(&self) -> StreamFormat {
        self.tap.format()
    }
    fn format_is_authoritative(&self) -> bool {
        self.tap.format_is_authoritative()
    }
    fn start(&mut self, sink: Box<dyn FrameSink>) -> Result<StreamFormat, TapError> {
        self.tap.start(sink)
    }
    fn stop(&mut self) -> Result<(), TapError> {
        self.gate.wait();
        self.tap.stop()
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_cleanup_keeps_the_lease_through_stop_and_watcher_drop() {
    for block_watcher_drop in [false, true] {
        let (tap, control, _, _) = fixture(0, 0);
        let (release, gate) = std::sync::mpsc::channel();
        let entered = Arc::new(AtomicBool::new(false));
        let gate = CleanupGate {
            gate: Some(gate),
            entered: entered.clone(),
        };
        let startup = murmur::capture::MicrophoneStartup::default();
        let input = murmur::capture::InputPreparation::new(move || {
            Ok(if block_watcher_drop {
                murmur::capture::PreparedInput::watched(tap, Box::new(CleanupGateWatch(gate)))
            } else {
                murmur::capture::PreparedInput::new(Box::new(StopGateTap { tap, gate }))
            })
        });
        let first = tokio::spawn(prepared_fixture(input, control, startup.clone()));
        tokio::time::timeout(Duration::from_secs(1), async {
            while !entered.load(Ordering::Acquire) {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        let attempted = Arc::new(AtomicUsize::new(0));
        let calls = attempted.clone();
        let result = prepared_fixture(
            murmur::capture::InputPreparation::new(move || {
                calls.fetch_add(1, Ordering::SeqCst);
                Err("unexpected native preparation during cleanup".into())
            }),
            Arc::new(AtomicU8::new(0)),
            startup,
        )
        .await;
        release.send(()).unwrap();
        let _ = first.await.unwrap();
        assert!(result.unwrap_err().contains("still pending"));
        assert_eq!(attempted.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test(flavor = "current_thread")]
async fn route_change_during_preparation_prevents_start_before_the_async_observer_runs() {
    let (tap, control, starts, stops) = fixture(0, 0);
    let changes = fotw_audio::device_change::DeviceChangeSignal::new();
    let notify = changes.clone();
    let (release, gate) = std::sync::mpsc::channel();
    let entered = Arc::new(AtomicBool::new(false));
    let preparing = entered.clone();
    let input = murmur::capture::InputPreparation::new(move || {
        preparing.store(true, Ordering::Release);
        let _ = gate.recv_timeout(Duration::from_secs(2));
        notify.raise(fotw_audio::device_change::DeviceChangeKind::DefaultInput);
        Ok(murmur::capture::PreparedInput::new(tap))
    });
    let attempt = tokio::spawn(murmur::capture::run_provider_with_preparation(
        input,
        murmur::capture::CaptureProvider::Deepgram {
            key: "fixture".into(),
            endpoint: DeepgramEndpoint::loopback(1),
        },
        control,
        Arc::new(AtomicBool::new(false)),
        Default::default(),
        Default::default(),
        changes,
        Default::default(),
    ));
    tokio::time::timeout(Duration::from_secs(1), async {
        while !entered.load(Ordering::Acquire) {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .unwrap();
    release.send(()).unwrap();
    // Hold only this fixture's async executor. The native worker must itself
    // reject the known route change instead of relying on a later async tick.
    std::thread::sleep(Duration::from_millis(50));
    assert!(
        attempt
            .await
            .unwrap()
            .unwrap_err()
            .contains("Microphone route changed")
    );
    assert_eq!(
        starts.load(Ordering::SeqCst),
        0,
        "a known-stale prepared device must never start"
    );
    assert_eq!(stops.load(Ordering::SeqCst), 1);
}

async fn quiet_provider() -> (DeepgramEndpoint, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let task = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let mut ws = accept_async(socket).await.unwrap();
        while let Some(Ok(message)) = ws.next().await {
            if let Message::Text(text) = message
                && text.contains("CloseStream")
            {
                let _ = ws.send(Message::text(r#"{"type":"Results","is_final":true,"speech_final":true,"channel":{"alternatives":[{"transcript":"Recovered fixture."}]}}"#)).await;
                ws.send(Message::text(r#"{"type":"Metadata"}"#))
                    .await
                    .unwrap();
                break;
            }
        }
    });
    (DeepgramEndpoint::loopback(port), task)
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn escape_during_native_stop_returns_before_cleanup_finishes() {
    let (endpoint, server) = quiet_provider().await;
    let (tap, control, _, _) = fixture(48000, 0);
    let (release, gate) = std::sync::mpsc::channel();
    let entered = Arc::new(AtomicBool::new(false));
    let tap = Box::new(StopGateTap {
        tap,
        gate: CleanupGate {
            gate: Some(gate),
            entered: entered.clone(),
        },
    });
    let attempt = tokio::spawn(murmur::capture::run_provider_with_startup(
        tap,
        murmur::capture::CaptureProvider::Deepgram {
            key: "fixture".into(),
            endpoint,
        },
        control.clone(),
        Arc::new(AtomicBool::new(false)),
        Default::default(),
    ));
    tokio::time::timeout(Duration::from_secs(1), async {
        while !entered.load(Ordering::Acquire) {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    let began = std::time::Instant::now();
    control.store(2, Ordering::Release);
    let result = attempt.await.unwrap();
    let elapsed = began.elapsed();
    let _ = release.send(());
    server.abort();
    assert!(
        elapsed < Duration::from_millis(400),
        "Escape waited for native stop: {elapsed:?}"
    );
    assert!(result.unwrap_err().contains("Cancelled"));
}

struct SlowCleanup {
    gate: std::sync::mpsc::Receiver<()>,
    entered: Arc<AtomicBool>,
}
impl SlowCleanup {
    fn wait(&self) {
        self.entered.store(true, Ordering::Release);
        let _ = self.gate.recv_timeout(Duration::from_secs(8));
    }
}
struct SlowStopTap(Box<dyn AudioTap>, SlowCleanup);
impl AudioTap for SlowStopTap {
    fn id(&self) -> &TapId {
        self.0.id()
    }
    fn format(&self) -> StreamFormat {
        self.0.format()
    }
    fn format_is_authoritative(&self) -> bool {
        self.0.format_is_authoritative()
    }
    fn start(&mut self, sink: Box<dyn FrameSink>) -> Result<StreamFormat, TapError> {
        self.0.start(sink)
    }
    fn stop(&mut self) -> Result<(), TapError> {
        self.1.wait();
        self.0.stop()
    }
}
struct SlowWatcher(SlowCleanup);
impl fotw_audio::DeviceWatch for SlowWatcher {}
impl Drop for SlowWatcher {
    fn drop(&mut self) {
        self.0.wait();
    }
}
async fn wait_for_input_idle(startup: &murmur::capture::MicrophoneStartup) {
    tokio::time::timeout(Duration::from_secs(1), async {
        while startup.state() != murmur::capture::MicrophoneState::Idle {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
}
#[tokio::test]
async fn stop_and_watcher_deadlines_preserve_pending_ownership_then_allow_fresh_speech() {
    use murmur::capture::{InputPreparation, MicrophoneState as Mic, PreparedInput};
    for watcher in [false, true] {
        let (endpoint, server) = quiet_provider().await;
        let (tap, control, _, stopped) = fixture(48000, 0);
        let (release, gate) = std::sync::mpsc::channel();
        let entered = Arc::new(AtomicBool::new(false));
        let gate = SlowCleanup {
            gate,
            entered: entered.clone(),
        };
        let input = InputPreparation::new(move || {
            Ok(if watcher {
                PreparedInput::watched(tap, Box::new(SlowWatcher(gate)))
            } else {
                PreparedInput::new(Box::new(SlowStopTap(tap, gate)))
            })
        });
        let startup = murmur::capture::MicrophoneStartup::default();
        let changes = fotw_audio::device_change::DeviceChangeSignal::new();
        let began = std::time::Instant::now();
        let first = tokio::spawn(murmur::capture::run_provider_with_preparation(
            input,
            murmur::capture::CaptureProvider::Deepgram {
                key: "fixture".into(),
                endpoint,
            },
            control,
            Arc::new(AtomicBool::new(false)),
            Default::default(),
            startup.clone(),
            changes.clone(),
            Default::default(),
        ));
        tokio::time::timeout(Duration::from_secs(1), async {
            while !entered.load(Ordering::Acquire) {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        if watcher {
            // Native Stop has succeeded, but its watcher destructor is wedged.
            // A later route event must not be mistaken for capture-time loss.
            changes.raise(fotw_audio::device_change::DeviceChangeKind::DefaultInput);
        }
        let error = first.await.unwrap().unwrap_err();
        let elapsed = began.elapsed();
        assert!(error.contains("cleanup timed out"), "{error}");
        assert!(elapsed < murmur::capture::MIC_STOP_DEADLINE + Duration::from_secs(1));
        assert_eq!(
            startup.state(),
            if watcher {
                Mic::CleaningUp
            } else {
                Mic::Stopping
            }
        );
        let calls = Arc::new(AtomicUsize::new(0));
        let called = calls.clone();
        let next = prepared_fixture(
            InputPreparation::new(move || {
                called.fetch_add(1, Ordering::SeqCst);
                Err("must not open during cleanup".into())
            }),
            Arc::new(AtomicU8::new(0)),
            startup.clone(),
        )
        .await;
        assert!(next.unwrap_err().contains("still pending"));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        release.send(()).unwrap();
        wait_for_input_idle(&startup).await;
        assert_eq!(stopped.load(Ordering::SeqCst), 1);
        server.abort();
        let (endpoint, server) = quiet_provider().await;
        let (tap, control, _, _) = fixture(48000, 0);
        assert_eq!(
            murmur::capture::run_provider_with_startup(
                tap,
                murmur::capture::CaptureProvider::Deepgram {
                    key: "fixture".into(),
                    endpoint
                },
                control,
                Arc::new(AtomicBool::new(false)),
                startup,
            )
            .await
            .unwrap(),
            "Recovered fixture."
        );
        server.await.unwrap();
    }
}
#[tokio::test]
async fn route_change_after_native_stop_during_watcher_cleanup_preserves_complete_speech() {
    use murmur::capture::{InputPreparation, MicrophoneState as Mic, PreparedInput};
    let (endpoint, server) = quiet_provider().await;
    let (tap, control, _, _) = fixture(48000, 0);
    let (release, gate) = std::sync::mpsc::channel();
    let entered = Arc::new(AtomicBool::new(false));
    let input = InputPreparation::new({
        let entered = entered.clone();
        move || {
            Ok(PreparedInput::watched(
                tap,
                Box::new(SlowWatcher(SlowCleanup { gate, entered })),
            ))
        }
    });
    let startup = murmur::capture::MicrophoneStartup::default();
    let changes = fotw_audio::device_change::DeviceChangeSignal::new();
    let first = tokio::spawn(murmur::capture::run_provider_with_preparation(
        input,
        murmur::capture::CaptureProvider::Deepgram {
            key: "fixture".into(),
            endpoint,
        },
        control,
        Arc::new(AtomicBool::new(false)),
        Default::default(),
        startup.clone(),
        changes.clone(),
        Default::default(),
    ));
    tokio::time::timeout(Duration::from_secs(1), async {
        while !entered.load(Ordering::Acquire) {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(startup.state(), Mic::CleaningUp);
    changes.raise(fotw_audio::device_change::DeviceChangeKind::DefaultInput);
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(!first.is_finished(), "watcher cleanup still owns the input");
    release.send(()).unwrap();
    assert_eq!(first.await.unwrap().unwrap(), "Recovered fixture.");
    assert_eq!(startup.state(), Mic::Idle);
    server.await.unwrap();
}
struct StopFailureTap(Box<dyn AudioTap>);
impl AudioTap for StopFailureTap {
    fn id(&self) -> &TapId {
        self.0.id()
    }
    fn format(&self) -> StreamFormat {
        self.0.format()
    }
    fn format_is_authoritative(&self) -> bool {
        self.0.format_is_authoritative()
    }
    fn start(&mut self, sink: Box<dyn FrameSink>) -> Result<StreamFormat, TapError> {
        self.0.start(sink)
    }
    fn stop(&mut self) -> Result<(), TapError> {
        Err(TapError::platform("fixture stop unconfirmed"))
    }
}
#[tokio::test]
async fn unconfirmed_stop_never_claims_idle_or_starts_another_input() {
    let (endpoint, server) = quiet_provider().await;
    let (tap, control, _, _) = fixture(48000, 0);
    let startup = murmur::capture::MicrophoneStartup::default();
    let error = murmur::capture::run_provider_with_startup(
        Box::new(StopFailureTap(tap)),
        murmur::capture::CaptureProvider::Deepgram {
            key: "fixture".into(),
            endpoint,
        },
        control,
        Arc::new(AtomicBool::new(false)),
        startup.clone(),
    )
    .await
    .unwrap_err();
    assert!(error.contains("stop unconfirmed"));
    assert_eq!(
        startup.state(),
        murmur::capture::MicrophoneState::Unconfirmed
    );
    let next = prepared_fixture(
        murmur::capture::InputPreparation::new(|| panic!("must not prepare another input")),
        Arc::new(AtomicU8::new(0)),
        startup,
    )
    .await;
    assert!(next.unwrap_err().contains("still pending"));
    server.abort();
}
#[tokio::test]
async fn aborting_the_async_attempt_never_runs_a_native_destructor_on_its_executor() {
    let (endpoint, server) = quiet_provider().await;
    let (tap, control, _, stopped) = fixture(48000, 0);
    let (release, gate) = std::sync::mpsc::channel();
    let entered = Arc::new(AtomicBool::new(false));
    let startup = murmur::capture::MicrophoneStartup::default();
    let first = tokio::spawn(murmur::capture::run_provider_with_startup(
        Box::new(StopGateTap {
            tap,
            gate: CleanupGate {
                gate: Some(gate),
                entered: entered.clone(),
            },
        }),
        murmur::capture::CaptureProvider::Deepgram {
            key: "fixture".into(),
            endpoint,
        },
        control,
        Arc::new(AtomicBool::new(false)),
        startup.clone(),
    ));
    tokio::time::timeout(Duration::from_secs(1), async {
        while !entered.load(Ordering::Acquire) {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    let began = std::time::Instant::now();
    first.abort();
    assert!(first.await.unwrap_err().is_cancelled());
    assert!(began.elapsed() < Duration::from_millis(400));
    assert_eq!(startup.state(), murmur::capture::MicrophoneState::Stopping);
    release.send(()).unwrap();
    wait_for_input_idle(&startup).await;
    assert_eq!(stopped.load(Ordering::SeqCst), 1);
    server.abort();
}

struct FailedWatcherCleanup;
impl fotw_audio::DeviceWatch for FailedWatcherCleanup {}
impl Drop for FailedWatcherCleanup {
    fn drop(&mut self) {
        panic!("fixture watcher cleanup failed after successful Stop");
    }
}
#[tokio::test]
async fn failed_watcher_cleanup_keeps_positive_stop_evidence_and_blocks_new_input() {
    let (endpoint, server) = quiet_provider().await;
    let (tap, control, _, stopped) = fixture(48000, 0);
    let startup = murmur::capture::MicrophoneStartup::default();
    let input = murmur::capture::InputPreparation::new(move || {
        Ok(murmur::capture::PreparedInput::watched(
            tap,
            Box::new(FailedWatcherCleanup),
        ))
    });
    let error = murmur::capture::run_provider_with_preparation(
        input,
        murmur::capture::CaptureProvider::Deepgram {
            key: "fixture".into(),
            endpoint,
        },
        control,
        Arc::new(AtomicBool::new(false)),
        Default::default(),
        startup.clone(),
        fotw_audio::device_change::DeviceChangeSignal::new(),
        Default::default(),
    )
    .await
    .unwrap_err();
    assert!(error.contains("cleanup"));
    assert_eq!(stopped.load(Ordering::SeqCst), 1);
    let notice =
        murmur::indicator::microphone_notice(startup.state(), murmur::core::Phase::Idle).unwrap();
    assert!(
        notice.contains("stopped"),
        "a confirmed Stop must remain visible despite watcher failure: {notice}"
    );
    let next = prepared_fixture(
        murmur::capture::InputPreparation::new(|| {
            panic!("cleanup failure must block new preparation")
        }),
        Arc::new(AtomicU8::new(0)),
        startup,
    )
    .await;
    assert!(next.unwrap_err().contains("still pending"));
    server.abort();
}
