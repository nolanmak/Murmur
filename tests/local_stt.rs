use murmur::local_stt::{LocalEndpoint, transcribe, wav16k};
use std::{
    sync::{Arc, atomic::AtomicU8},
    time::Duration,
};
#[test]
fn endpoints_reject_remote_credentials_and_paths() {
    for url in [
        "http://example.com:8080",
        "http://127.0.0.1.evil:8080",
        "http://user@127.0.0.1:8080",
        "https://127.0.0.1:8080",
        "http://127.0.0.1:8080/evil",
    ] {
        assert!(LocalEndpoint::new(url).is_err(), "{url}");
    }
    assert!(LocalEndpoint::new("http://127.0.0.1:8081").is_ok());
}
#[test]
fn wav_is_mono_linear16_at_16k() {
    let wav = wav16k(&[1, -2]);
    assert_eq!(&wav[..4], b"RIFF");
    assert_eq!(&wav[8..12], b"WAVE");
    assert_eq!(u32::from_le_bytes(wav[24..28].try_into().unwrap()), 16000);
    assert_eq!(&wav[44..], &[1, 0, 254, 255]);
}
#[tokio::test]
async fn silence_and_precancel_do_not_connect() {
    let endpoint = LocalEndpoint::new("http://127.0.0.1:1").unwrap();
    assert_eq!(
        transcribe(
            &endpoint,
            &[0; 16000],
            Arc::new(AtomicU8::new(1)),
            Duration::from_secs(1)
        )
        .await
        .unwrap(),
        ""
    );
    assert!(
        transcribe(
            &endpoint,
            &[1000; 16000],
            Arc::new(AtomicU8::new(2)),
            Duration::from_secs(1)
        )
        .await
        .unwrap_err()
        .contains("Cancelled")
    );
}

#[test]
fn provider_settings_roundtrip_and_invalid_endpoint_preserves_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("spaces directory/speech.json");
    let mut settings = murmur::speech::Settings::load(&path).unwrap();
    settings.provider = murmur::speech::Provider::LocalWhisper;
    settings.save(&path).unwrap();
    assert_eq!(murmur::speech::Settings::load(&path).unwrap(), settings);
    settings.local_endpoint = "http://remote.example".into();
    assert!(settings.save(&path).is_err());
    assert_eq!(
        murmur::speech::Settings::load(&path).unwrap().provider,
        murmur::speech::Provider::LocalWhisper
    );
}

async fn fixture(status: &str, body: &str) -> (LocalEndpoint, tokio::task::JoinHandle<Vec<u8>>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint =
        LocalEndpoint::new(&format!("http://{}", listener.local_addr().unwrap())).unwrap();
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nLocation: http://127.0.0.1:1/\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let task = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut received = Vec::new();
        loop {
            let mut buf = [0; 8192];
            let n = socket.read(&mut buf).await.unwrap();
            if n == 0 {
                break;
            }
            received.extend_from_slice(&buf[..n]);
            if let Some(end) = received.windows(4).position(|s| s == b"\r\n\r\n") {
                let header = String::from_utf8_lossy(&received[..end]).to_lowercase();
                let len = header
                    .lines()
                    .find_map(|l| {
                        l.strip_prefix("content-length: ")
                            .and_then(|v| v.parse::<usize>().ok())
                    })
                    .unwrap();
                if received.len() >= end + 4 + len {
                    break;
                }
            }
        }
        socket.write_all(response.as_bytes()).await.unwrap();
        received
    });
    (endpoint, task)
}
#[tokio::test]
async fn sends_whisper_wav_contract_and_trims_text() {
    let (endpoint, server) = fixture("200 OK", r#"{"text":"  Fixture speech.\n"}"#).await;
    assert_eq!(
        transcribe(
            &endpoint,
            &[1000; 1600],
            Arc::new(AtomicU8::new(1)),
            Duration::from_secs(2)
        )
        .await
        .unwrap(),
        "Fixture speech."
    );
    let bytes = server.await.unwrap();
    let text = String::from_utf8_lossy(&bytes);
    assert!(text.starts_with("POST /inference "));
    assert!(text.contains("name=\"file\"; filename=\"audio.wav\""));
    assert!(text.contains("RIFF"));
    assert!(!text.contains("Authorization:"));
}
#[tokio::test]
async fn rejects_redirects_and_malformed_responses() {
    for (status, body) in [
        ("302 Found", ""),
        ("200 OK", "bad json"),
        ("200 OK", r#"{"other":"no text"}"#),
        ("503 Unavailable", ""),
    ] {
        let (endpoint, server) = fixture(status, body).await;
        assert!(
            transcribe(
                &endpoint,
                &[1000; 1600],
                Arc::new(AtomicU8::new(1)),
                Duration::from_secs(2)
            )
            .await
            .is_err()
        );
        server.await.unwrap();
    }
}
#[test]
fn local_selection_never_resolves_cloud_credentials() {
    let settings = murmur::speech::Settings {
        provider: murmur::speech::Provider::LocalWhisper,
        ..Default::default()
    };
    assert!(matches!(
        settings
            .capture_provider(|| panic!("local mode touched credentials"))
            .unwrap(),
        murmur::capture::CaptureProvider::Local(_)
    ));
    let settings = murmur::speech::Settings::default();
    assert!(
        settings
            .capture_provider(|| Err("fixture missing key".into()))
            .is_err()
    );
}
#[tokio::test]
async fn timeout_and_cancellation_bound_unresponsive_inference() {
    use std::sync::atomic::Ordering;
    for cancel in [false, true] {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint =
            LocalEndpoint::new(&format!("http://{}", listener.local_addr().unwrap())).unwrap();
        let control = Arc::new(AtomicU8::new(1));
        let signal = control.clone();
        let server = tokio::spawn(async move {
            let (_socket, _) = listener.accept().await.unwrap();
            if cancel {
                signal.store(2, Ordering::Release);
            }
            tokio::time::sleep(Duration::from_secs(5)).await;
        });
        let result = transcribe(&endpoint, &[1000; 1600], control, Duration::from_millis(80))
            .await
            .unwrap_err();
        assert!(
            result.contains(if cancel { "Cancelled" } else { "timed out" }),
            "{result}"
        );
        server.abort();
    }
}
#[tokio::test]
async fn rejects_oversize_audio_without_connecting() {
    let endpoint = LocalEndpoint::new("http://127.0.0.1:1").unwrap();
    assert!(
        transcribe(
            &endpoint,
            &vec![1000; 16000 * 121],
            Arc::new(AtomicU8::new(1)),
            Duration::from_secs(1)
        )
        .await
        .unwrap_err()
        .contains("120-second")
    );
}
#[tokio::test]
async fn rejects_oversize_response() {
    let (endpoint, server) = fixture("200 OK", &"x".repeat(1024 * 1024 + 1)).await;
    assert!(
        transcribe(
            &endpoint,
            &[1000; 1600],
            Arc::new(AtomicU8::new(1)),
            Duration::from_secs(2)
        )
        .await
        .unwrap_err()
        .contains("too large")
    );
    server.await.unwrap();
}

#[test]
fn invalid_settings_block_capture_until_explicit_saved_choice() {
    use murmur::speech::{Provider, Selection};
    for bytes in [
        b"invalid json".as_slice(),
        br#"{"provider":"local-whisper","local_endpoint":"http://remote.example"}"#,
    ] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("speech.json");
        std::fs::write(&path, bytes).unwrap();
        let mut selection = Selection::load(Ok(path.clone()));
        assert!(selection.error().is_some());
        assert!(
            selection
                .capture_provider(|| panic!("blocked settings accessed credentials"))
                .is_err()
        );
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        selection.choose(Provider::LocalWhisper, &path).unwrap();
        assert!(selection.error().is_none());
        assert!(matches!(
            selection
                .capture_provider(|| panic!("local choice accessed credentials"))
                .unwrap(),
            murmur::capture::CaptureProvider::Local(_)
        ));
    }
}
#[test]
fn unreadable_settings_and_failed_repair_remain_blocked() {
    use murmur::speech::{Provider, Selection};
    let dir = tempfile::tempdir().unwrap();
    let mut selection = Selection::load(Ok(dir.path().to_owned()));
    assert!(selection.error().is_some());
    assert!(
        selection
            .choose(Provider::LocalWhisper, dir.path())
            .is_err()
    );
    assert!(
        selection
            .capture_provider(|| panic!("failed repair accessed credentials"))
            .is_err()
    );
    assert!(
        Selection::load(Err("Cannot locate home directory".into()))
            .error()
            .is_some()
    );
}

#[test]
fn fallback_selection_persists_and_missing_cloud_key_uses_local() {
    let settings: murmur::speech::Settings = serde_json::from_value(serde_json::json!({
        "provider": "deepgram-then-local", "local_endpoint": "http://127.0.0.1:17863"
    }))
    .expect("explicit fallback selection must be supported");
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("speech.json");
    settings.save(&path).unwrap();
    let restored = murmur::speech::Settings::load(&path).unwrap();
    assert_eq!(restored, settings);
    assert!(matches!(
        restored
            .capture_provider(|| Err("No configured key".into()))
            .unwrap(),
        murmur::capture::CaptureProvider::Local(_)
    ));
}

async fn language_fixture(
    health: serde_json::Value,
    inference_headers: Option<String>,
) -> (LocalEndpoint, tokio::task::JoinHandle<Vec<u8>>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint =
        LocalEndpoint::new(&format!("http://{}", listener.local_addr().unwrap())).unwrap();
    let task = tokio::spawn(async move {
        let replies = std::iter::once((health.to_string(), String::new()))
            .chain(inference_headers.map(|headers| (r#"{"text":"Bonjour."}"#.into(), headers)));
        let mut last_request = Vec::new();
        for (body, headers) in replies {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut bytes = Vec::new();
            loop {
                let mut chunk = [0; 8192];
                let n = socket.read(&mut chunk).await.unwrap();
                assert_ne!(n, 0, "unexpected request EOF");
                bytes.extend_from_slice(&chunk[..n]);
                if let Some(end) = bytes.windows(4).position(|s| s == b"\r\n\r\n") {
                    let header = String::from_utf8_lossy(&bytes[..end]).to_lowercase();
                    let len = header
                        .lines()
                        .find_map(|line| {
                            line.strip_prefix("content-length: ")?.parse::<usize>().ok()
                        })
                        .unwrap_or(0);
                    if bytes.len() >= end + 4 + len {
                        break;
                    }
                }
            }
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n{headers}\r\n{body}",
                body.len()
            );
            socket.write_all(response.as_bytes()).await.unwrap();
            last_request = bytes;
        }
        last_request
    });
    (endpoint, task)
}

fn language_health() -> serde_json::Value {
    serde_json::json!({"status":"ok", "murmur_capabilities": {
        "protocol_version":1, "instance_id":"a".repeat(64),
        "model_sha256":"b".repeat(64), "languages":["en","fr"]
    }})
}

#[tokio::test]
async fn local_language_preflight_rejects_missing_unsupported_or_malformed_capabilities() {
    use murmur::local_stt::prepare;
    let mut unsupported = language_health();
    unsupported["murmur_capabilities"]["languages"] = serde_json::json!(["en"]);
    let mut wrong_version = language_health();
    wrong_version["murmur_capabilities"]["protocol_version"] = 2.into();
    let mut bad_instance = language_health();
    bad_instance["murmur_capabilities"]["instance_id"] = "not-an-instance".into();
    let mut repeated = language_health();
    repeated["murmur_capabilities"]["languages"] = serde_json::json!(["en", "fr", "fr"]);
    let mut huge = language_health();
    huge["padding"] = "x".repeat(16 * 1024).into();
    for health in [
        serde_json::json!({"status":"ok"}),
        unsupported,
        wrong_version,
        bad_instance,
        repeated,
        huge,
    ] {
        let (endpoint, server) = language_fixture(health, None).await;
        assert!(
            prepare(&endpoint, "fr", Duration::from_secs(1))
                .await
                .is_err()
        );
        let request = server.await.unwrap();
        assert!(request.starts_with(b"GET /health "));
    }
    let (endpoint, server) = language_fixture(serde_json::json!({"status":"ok"}), None).await;
    assert_eq!(
        prepare(&endpoint, "en", Duration::from_secs(1))
            .await
            .unwrap()
            .language(),
        "en"
    );
    server.await.unwrap();
}

#[tokio::test]
async fn local_inference_binds_language_instance_and_model_and_rejects_changed_runtime() {
    let expected_headers = format!(
        "X-Murmur-Instance: {}\r\nX-Murmur-Model: {}\r\n",
        "a".repeat(64),
        "b".repeat(64)
    );
    let changed_headers = format!(
        "X-Murmur-Instance: {}\r\nX-Murmur-Model: {}\r\n",
        "c".repeat(64),
        "b".repeat(64)
    );
    let changed_model = format!(
        "X-Murmur-Instance: {}\r\nX-Murmur-Model: {}\r\n",
        "a".repeat(64),
        "c".repeat(64)
    );
    for (headers, succeeds) in [
        (expected_headers, true),
        (changed_headers, false),
        (changed_model, false),
        (String::new(), false),
    ] {
        let (endpoint, server) = language_fixture(language_health(), Some(headers)).await;
        let bound = murmur::local_stt::prepare(&endpoint, "fr", Duration::from_secs(1))
            .await
            .unwrap();
        let result = transcribe(
            &bound,
            &[1000; 1600],
            Arc::new(AtomicU8::new(1)),
            Duration::from_secs(1),
        )
        .await;
        if succeeds {
            assert_eq!(result.unwrap(), "Bonjour.");
        } else {
            assert!(result.unwrap_err().contains("changed after preflight"));
        }
        let request = server.await.unwrap();
        let text = String::from_utf8_lossy(&request);
        assert!(text.contains("name=\"language\"\r\n\r\nfr\r\n"));
        assert!(text.contains("name=\"translate\"\r\n\r\nfalse\r\n"));
        assert!(text.contains("name=\"detect_language\"\r\n\r\nfalse\r\n"));
        assert!(text.contains(&format!("x-murmur-instance: {}", "a".repeat(64))));
        assert!(text.contains(&format!("x-murmur-model: {}", "b".repeat(64))));
    }
}
