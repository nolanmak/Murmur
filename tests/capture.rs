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
