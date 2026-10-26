use murmur::local_stt::{LocalEndpoint, prepare, transcribe, wav16k};
use std::sync::{Arc, atomic::AtomicU8};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const ID: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

async fn receive(socket: &mut tokio::net::TcpStream) -> Vec<u8> {
    let mut request = Vec::new();
    loop {
        let mut buffer = [0; 8192];
        let count = tokio::time::timeout(Duration::from_secs(3), socket.read(&mut buffer))
            .await
            .unwrap()
            .unwrap();
        assert_ne!(count, 0);
        request.extend_from_slice(&buffer[..count]);
        if let Some(end) = request.windows(4).position(|b| b == b"\r\n\r\n") {
            let headers = String::from_utf8_lossy(&request[..end]).to_lowercase();
            let length = headers
                .lines()
                .find_map(|s| s.strip_prefix("content-length: "))
                .map(|s| s.parse::<usize>().unwrap())
                .unwrap_or(0);
            if request.len() >= end + 4 + length {
                break;
            }
        }
    }
    request
}

async fn server(
    replies: Vec<(&'static str, String, String)>,
) -> (LocalEndpoint, tokio::task::JoinHandle<Vec<Vec<u8>>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint =
        LocalEndpoint::new(&format!("http://{}", listener.local_addr().unwrap())).unwrap();
    let task = tokio::spawn(async move {
        let mut requests = Vec::new();
        for (status, headers, body) in replies {
            let (mut socket, _) = tokio::time::timeout(Duration::from_secs(3), listener.accept())
                .await
                .unwrap()
                .unwrap();
            let request = receive(&mut socket).await;
            let response = format!(
                "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n{headers}\r\n{body}",
                body.len()
            );
            socket.write_all(response.as_bytes()).await.unwrap();
            requests.push(request);
        }
        requests
    });
    (endpoint, task)
}

fn busy() -> (&'static str, String, String) {
    (
        "429 Too Many Requests",
        String::new(),
        r#"{"text":"Busy response text must never be delivered"}"#.into(),
    )
}

#[tokio::test]
async fn busy_recovery_preserves_the_entire_utterance_language_and_model_binding() {
    let health=serde_json::json!({"status":"ok","murmur_capabilities":{"protocol_version":1,"instance_id":ID,"model_sha256":ID,"languages":["es"]}}).to_string();
    let headers = format!("X-Murmur-Instance: {ID}\r\nX-Murmur-Model: {ID}\r\n");
    let (endpoint, task) = server(vec![
        ("200 OK", String::new(), health),
        busy(),
        busy(),
        (
            "200 OK",
            headers,
            r#"{"text":"Complete fixture speech"}"#.into(),
        ),
    ])
    .await;
    let endpoint = prepare(&endpoint, "es", Duration::from_secs(2))
        .await
        .unwrap();
    let pcm: Vec<i16> = (0..16000)
        .map(|i| ((i * 17) % 30001 - 15000) as i16)
        .collect();
    let text = transcribe(
        &endpoint,
        &pcm,
        Arc::new(AtomicU8::new(1)),
        Duration::from_secs(2),
    )
    .await
    .unwrap();
    assert_eq!(text, "Complete fixture speech");
    let requests = task.await.unwrap();
    assert_eq!(requests.len(), 4);
    let wav = wav16k(&pcm);
    for request in &requests[1..] {
        assert!(request.windows(wav.len()).any(|b| b == wav));
        let text = String::from_utf8_lossy(request).to_lowercase();
        assert!(text.starts_with("post /inference "));
        assert!(text.contains(&format!("x-murmur-instance: {ID}")));
        assert!(text.contains(&format!("x-murmur-model: {ID}")));
        assert!(text.contains("name=\"language\"\r\n\r\nes\r\n"));
        assert!(!text.contains("authorization:"));
    }
}

#[tokio::test]
async fn persistent_busy_is_bounded_and_names_a_manual_recovery_action() {
    let (endpoint, task) = server((0..6).map(|_| busy()).collect()).await;
    let error = transcribe(
        &endpoint,
        &[1000; 1600],
        Arc::new(AtomicU8::new(1)),
        Duration::from_secs(3),
    )
    .await
    .unwrap_err();
    assert!(
        error.contains("busy") && error.contains("try again"),
        "{error}"
    );
    assert_eq!(task.await.unwrap().len(), 6);
}

#[tokio::test]
async fn a_long_retry_after_is_never_retried_early() {
    let (endpoint, task) = server(vec![(
        "429 Too Many Requests",
        "Retry-After: 2\r\n".into(),
        String::new(),
    )])
    .await;
    let error = transcribe(
        &endpoint,
        &[1000; 1600],
        Arc::new(AtomicU8::new(1)),
        Duration::from_secs(3),
    )
    .await
    .unwrap_err();
    assert!(error.contains("busy"), "{error}");
    assert_eq!(task.await.unwrap().len(), 1);
}

#[tokio::test]
async fn busy_retries_share_one_total_inference_deadline() {
    let (endpoint, task) = server((0..6).map(|_| busy()).collect()).await;
    let error = transcribe(
        &endpoint,
        &[1000; 1600],
        Arc::new(AtomicU8::new(1)),
        Duration::from_millis(150),
    )
    .await
    .unwrap_err();
    task.abort();
    let _ = task.await;
    assert!(error.contains("timed out"), "{error}");
}

#[tokio::test]
async fn other_errors_are_not_retried_as_busy() {
    let (endpoint, task) = server(vec![("503 Unavailable", String::new(), String::new())]).await;
    let error = transcribe(
        &endpoint,
        &[1000; 1600],
        Arc::new(AtomicU8::new(1)),
        Duration::from_secs(2),
    )
    .await
    .unwrap_err();
    assert!(error.contains("rejected"), "{error}");
    assert_eq!(task.await.unwrap().len(), 1);
}

#[tokio::test]
async fn cancellation_after_busy_refusal_never_issues_another_request() {
    use std::sync::atomic::Ordering;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint =
        LocalEndpoint::new(&format!("http://{}", listener.local_addr().unwrap())).unwrap();
    let (sent_tx, sent_rx) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let request = receive(&mut socket).await;
        socket
            .write_all(
                b"HTTP/1.1 429 Too Many Requests\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            )
            .await
            .unwrap();
        sent_tx.send(()).unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(300), listener.accept())
                .await
                .is_err(),
            "cancelled busy waits must not submit another request"
        );
        request
    });
    let control = Arc::new(AtomicU8::new(1));
    let cancel = control.clone();
    let client = tokio::spawn(async move {
        transcribe(&endpoint, &[1000; 1600], control, Duration::from_secs(2)).await
    });
    tokio::time::timeout(Duration::from_secs(2), sent_rx)
        .await
        .unwrap()
        .unwrap();
    cancel.store(2, Ordering::Release);
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), client)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err(),
        "Cancelled"
    );
    assert!(server.await.unwrap().starts_with(b"POST /inference "));
}

#[tokio::test]
async fn a_runtime_change_after_busy_never_adopts_the_replacement_model() {
    let health=serde_json::json!({"status":"ok","murmur_capabilities":{"protocol_version":1,"instance_id":ID,"model_sha256":ID,"languages":["en"]}}).to_string();
    let replacement = "b".repeat(64);
    let (endpoint, task) = server(vec![
        ("200 OK", String::new(), health),
        busy(),
        (
            "200 OK",
            format!("X-Murmur-Instance: {replacement}\r\nX-Murmur-Model: {replacement}\r\n"),
            r#"{"text":"Must not use replacement model text"}"#.into(),
        ),
    ])
    .await;
    let endpoint = prepare(&endpoint, "en", Duration::from_secs(2))
        .await
        .unwrap();
    let error = transcribe(
        &endpoint,
        &[1000; 1600],
        Arc::new(AtomicU8::new(1)),
        Duration::from_secs(2),
    )
    .await
    .unwrap_err();
    assert!(error.contains("changed after preflight"), "{error}");
    assert_eq!(task.await.unwrap().len(), 3);
}
