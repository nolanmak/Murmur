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
