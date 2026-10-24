use futures_util::{SinkExt, StreamExt};
use murmur::{
    fallback::{self, Deadlines},
    local_stt::LocalEndpoint,
};
use std::sync::{
    Arc,
    atomic::{AtomicU8, Ordering},
};
use std::time::Duration;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::mpsc,
};
use tokio_tungstenite::{accept_async, tungstenite::Message};

fn deadlines() -> Deadlines {
    Deadlines {
        connect: Duration::from_millis(100),
        cloud_final: Duration::from_millis(100),
        local: Duration::from_secs(2),
    }
}
async fn local_server() -> (LocalEndpoint, tokio::task::JoinHandle<Vec<u8>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint =
        LocalEndpoint::new(&format!("http://{}", listener.local_addr().unwrap())).unwrap();
    let task = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut bytes = Vec::new();
        loop {
            let mut buf = [0; 8192];
            let n = socket.read(&mut buf).await.unwrap();
            assert!(n > 0);
            bytes.extend_from_slice(&buf[..n]);
            if let Some(end) = bytes.windows(4).position(|v| v == b"\r\n\r\n") {
                let header = String::from_utf8_lossy(&bytes[..end]).to_lowercase();
                let len: usize = header
                    .lines()
                    .find_map(|l| l.strip_prefix("content-length: "))
                    .unwrap()
                    .parse()
                    .unwrap();
                if bytes.len() >= end + 4 + len {
                    break;
                }
            }
        }
        let body = r#"{"text":"Complete local utterance."}"#;
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
        bytes
    });
    (endpoint, task)
}
#[tokio::test]
async fn disconnected_cloud_retries_the_entire_utterance_including_later_audio() {
    let (local, server) = local_server().await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let cloud = fotw_stt::DeepgramEndpoint::loopback(listener.local_addr().unwrap().port());
    let (tx, rx) = mpsc::channel(8);
    tx.send(vec![1000; 1600]).await.unwrap();
    let producer = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let mut ws = accept_async(socket).await.unwrap();
        while let Some(Ok(message)) = ws.next().await {
            if matches!(message, Message::Binary(_)) {
                break;
            }
        }
        ws.send(Message::text(r#"{"type":"Results","is_final":true,"speech_final":true,"channel":{"alternatives":[{"transcript":"Incomplete cloud text."}]}}"#)).await.unwrap();
        ws.close(None).await.unwrap();
        tokio::time::sleep(Duration::from_millis(30)).await;
        tx.send(vec![2000; 1600])
            .await
            .expect("capture must continue after network failure");
    });
    let result = tokio::time::timeout(
        Duration::from_secs(3),
        fallback::transcribe(
            "fixture".into(),
            rx,
            cloud,
            local,
            Arc::new(AtomicU8::new(0)),
            deadlines(),
        ),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(result, "Complete local utterance.");
    producer.await.unwrap();
    let bytes = server.await.unwrap();
    let start = bytes.windows(4).position(|v| v == b"RIFF").unwrap();
    assert_eq!(
        u32::from_le_bytes(bytes[start + 40..start + 44].try_into().unwrap()),
        6400
    );
    assert_eq!(
        &bytes[start + 44..start + 3244],
        &[1000i16.to_le_bytes(); 1600].concat()
    );
    assert_eq!(
        &bytes[start + 3244..start + 6444],
        &[2000i16.to_le_bytes(); 1600].concat()
    );
}
#[tokio::test]
async fn cloud_success_does_not_invoke_local() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let cloud = fotw_stt::DeepgramEndpoint::loopback(listener.local_addr().unwrap().port());
    let server = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let mut ws = accept_async(socket).await.unwrap();
        while let Some(Ok(msg)) = ws.next().await {
            if let Message::Text(s) = msg
                && s.contains("CloseStream")
            {
                ws.send(Message::text(r#"{"type":"Results","is_final":true,"speech_final":true,"channel":{"alternatives":[{"transcript":"Cloud final."}]}}"#)).await.unwrap();
                ws.send(Message::text(r#"{"type":"Metadata"}"#))
                    .await
                    .unwrap();
                break;
            }
        }
    });
    let (tx, rx) = mpsc::channel(2);
    tx.send(vec![1000; 1600]).await.unwrap();
    drop(tx);
    let result = fallback::transcribe(
        "fixture".into(),
        rx,
        cloud,
        LocalEndpoint::new("http://127.0.0.1:1").unwrap(),
        Arc::new(AtomicU8::new(1)),
        deadlines(),
    )
    .await
    .unwrap();
    assert_eq!(result, "Cloud final.");
    server.await.unwrap();
}
#[tokio::test]
async fn cancellation_while_waiting_for_release_does_not_invoke_local() {
    let (tx, rx) = mpsc::channel(2);
    tx.send(vec![1000; 1600]).await.unwrap();
    let control = Arc::new(AtomicU8::new(0));
    let signal = control.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(30)).await;
        signal.store(2, Ordering::Release);
    });
    let result = tokio::time::timeout(
        Duration::from_millis(300),
        fallback::transcribe(
            "fixture".into(),
            rx,
            fotw_stt::DeepgramEndpoint::loopback(1),
            LocalEndpoint::new("http://127.0.0.1:1").unwrap(),
            control,
            deadlines(),
        ),
    )
    .await
    .unwrap();
    assert!(result.unwrap_err().contains("Cancelled"));
    drop(tx);
}

#[tokio::test]
async fn invalid_cloud_request_is_surfaced_instead_of_hidden_by_local_fallback() {
    let (local, local_task) = local_server().await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let cloud = fotw_stt::DeepgramEndpoint::loopback(listener.local_addr().unwrap().port());
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut buffer = [0; 4096];
        let _ = socket.read(&mut buffer).await.unwrap();
        socket
            .write_all(
                b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            )
            .await
            .unwrap();
    });
    let (tx, rx) = mpsc::channel(2);
    tx.send(vec![1000; 1600]).await.unwrap();
    drop(tx);
    let result = fallback::transcribe(
        "fixture".into(),
        rx,
        cloud,
        local,
        Arc::new(AtomicU8::new(1)),
        deadlines(),
    )
    .await;
    local_task.abort();
    server.await.unwrap();
    assert!(
        result.is_err(),
        "invalid requests must not be disguised as successful local transcription: {result:?}"
    );
}

#[tokio::test]
async fn stalled_cloud_final_retries_locally_within_its_deadline() {
    let (local, local_task) = local_server().await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let cloud = fotw_stt::DeepgramEndpoint::loopback(listener.local_addr().unwrap().port());
    let server = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let mut ws = accept_async(socket).await.unwrap();
        while let Some(Ok(msg)) = ws.next().await {
            if let Message::Text(s) = msg
                && s.contains("CloseStream")
            {
                tokio::time::sleep(Duration::from_secs(5)).await;
                break;
            }
        }
    });
    let (tx, rx) = mpsc::channel(2);
    tx.send(vec![1000; 1600]).await.unwrap();
    drop(tx);
    let result = tokio::time::timeout(
        Duration::from_secs(1),
        fallback::transcribe(
            "fixture".into(),
            rx,
            cloud,
            local,
            Arc::new(AtomicU8::new(1)),
            deadlines(),
        ),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(result, "Complete local utterance.");
    local_task.await.unwrap();
    server.abort();
}

#[tokio::test]
async fn transient_cloud_failure_gets_one_bounded_retry_with_the_whole_utterance() {
    let (local, local_task) = local_server().await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let cloud = fotw_stt::DeepgramEndpoint::loopback(listener.local_addr().unwrap().port());
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut buf = [0; 4096];
        let _ = socket.read(&mut buf).await.unwrap();
        socket
            .write_all(
                b"HTTP/1.1 503 Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            )
            .await
            .unwrap();
        drop(socket);
        let (socket, _) = listener.accept().await.unwrap();
        let mut ws = accept_async(socket).await.unwrap();
        let mut heard = 0;
        while let Some(Ok(msg)) = ws.next().await {
            match msg {
                Message::Binary(b) => heard += b.len(),
                Message::Text(s) if s.contains("CloseStream") => {
                    assert_eq!(heard, 3200);
                    ws.send(Message::text(r#"{"type":"Results","is_final":true,"speech_final":true,"channel":{"alternatives":[{"transcript":"Retried cloud final."}]}}"#)).await.unwrap();
                    ws.send(Message::text(r#"{"type":"Metadata"}"#))
                        .await
                        .unwrap();
                    break;
                }
                _ => {}
            }
        }
    });
    let (tx, rx) = mpsc::channel(2);
    tx.send(vec![1000; 1600]).await.unwrap();
    drop(tx);
    let policy = Deadlines {
        connect: Duration::from_millis(500),
        cloud_final: Duration::from_secs(1),
        ..deadlines()
    };
    let result = fallback::transcribe(
        "fixture".into(),
        rx,
        cloud,
        local,
        Arc::new(AtomicU8::new(1)),
        policy,
    )
    .await
    .unwrap();
    local_task.abort();
    server.abort();
    assert_eq!(result, "Retried cloud final.");
}

#[tokio::test]
async fn auth_and_long_rate_limit_use_local_without_retrying_cloud() {
    for response in [
        "HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        "HTTP/1.1 429 Too Many Requests\r\nRetry-After: 60\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
    ] {
        let (local, local_task) = local_server().await;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let cloud = fotw_stt::DeepgramEndpoint::loopback(listener.local_addr().unwrap().port());
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buf = [0; 4096];
            let _ = socket.read(&mut buf).await.unwrap();
            socket.write_all(response.as_bytes()).await.unwrap();
            drop(socket);
            assert!(
                tokio::time::timeout(Duration::from_millis(250), listener.accept())
                    .await
                    .is_err(),
                "must not retry auth or ignore Retry-After"
            );
        });
        let (tx, rx) = mpsc::channel(2);
        tx.send(vec![1000; 1600]).await.unwrap();
        drop(tx);
        let result = fallback::transcribe(
            "fixture".into(),
            rx,
            cloud,
            local,
            Arc::new(AtomicU8::new(1)),
            deadlines(),
        )
        .await
        .unwrap();
        assert_eq!(result, "Complete local utterance.");
        local_task.await.unwrap();
        server.await.unwrap();
    }
}

#[tokio::test]
async fn fallback_retains_at_most_the_capture_limit() {
    let (local, server) = local_server().await;
    let (tx, rx) = mpsc::channel(2);
    tx.send(vec![1000; 16_000 * 121]).await.unwrap();
    drop(tx);
    let progress = fallback::Progress::default();
    let result = fallback::transcribe_observed(
        "fixture".into(),
        rx,
        fotw_stt::DeepgramEndpoint::loopback(1),
        local,
        Arc::new(AtomicU8::new(1)),
        deadlines(),
        progress.clone(),
    )
    .await
    .unwrap();
    assert_eq!(result, "Complete local utterance.");
    assert!(matches!(progress.stage(), fallback::Stage::Local));
    let bytes = server.await.unwrap();
    let start = bytes.windows(4).position(|v| v == b"RIFF").unwrap();
    assert_eq!(
        u32::from_le_bytes(bytes[start + 40..start + 44].try_into().unwrap()),
        16_000 * 120 * 2
    );
}

#[tokio::test]
async fn local_fallback_reports_unsupported_hints_without_sending_them_to_the_local_engine() {
    let (local, server) = local_server().await;
    let (tx, rx) = mpsc::channel(1);
    tx.send(vec![1000; 1600]).await.unwrap();
    drop(tx);
    let progress = fallback::Progress::default();
    let text = fallback::transcribe_with_vocabulary(
        "fixture".into(),
        rx,
        fotw_stt::DeepgramEndpoint::loopback(1),
        local,
        Arc::new(AtomicU8::new(1)),
        deadlines(),
        progress.clone(),
        &["Private fixture name".into()],
    )
    .await
    .unwrap();
    assert_eq!(text, "Complete local utterance.");
    assert!(matches!(
        progress.stage(),
        fallback::Stage::LocalWithoutHints
    ));
    assert!(
        progress
            .stage()
            .completion_message("Inserted")
            .contains("hints unavailable")
    );
    let request = server.await.unwrap();
    assert!(!String::from_utf8_lossy(&request).contains("Private fixture name"));
}

#[tokio::test]
#[allow(clippy::result_large_err, reason = "tungstenite callback error type")]
async fn cloud_retry_reuses_the_same_recognition_hints() {
    use tokio_tungstenite::{
        accept_hdr_async,
        tungstenite::handshake::server::{Request, Response},
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = fotw_stt::DeepgramEndpoint::loopback(listener.local_addr().unwrap().port());
    let server = tokio::spawn(async move {
        for attempt in 0..2 {
            let (socket, _) = listener.accept().await.unwrap();
            let websocket = accept_hdr_async(socket, |req: &Request, res: Response| {
                let url = reqwest::Url::parse(&format!("http://localhost{}", req.uri())).unwrap();
                assert_eq!(
                    url.query_pairs()
                        .filter(|(k, _)| k == "keyterm")
                        .map(|(_, v)| v.into_owned())
                        .collect::<Vec<_>>(),
                    vec!["C++", "Project Acorn"]
                );
                if attempt == 0 {
                    let mut denied = tokio_tungstenite::tungstenite::http::Response::new(Some(
                        "fixture outage".to_owned(),
                    ));
                    *denied.status_mut() =
                        tokio_tungstenite::tungstenite::http::StatusCode::SERVICE_UNAVAILABLE;
                    return Err(denied);
                }
                Ok(res)
            })
            .await;
            if attempt == 0 {
                assert!(websocket.is_err());
                continue;
            }
            let mut websocket = websocket.unwrap();
            while let Some(Ok(message)) = websocket.next().await {
                if matches!(message,Message::Text(ref text) if text.contains("CloseStream")) {
                    websocket.send(Message::text(r#"{"type":"Results","is_final":true,"speech_final":true,"channel":{"alternatives":[{"transcript":"Cloud fixture."}]}}"#)).await.unwrap();
                    websocket
                        .send(Message::text(r#"{"type":"Metadata"}"#))
                        .await
                        .unwrap();
                    break;
                }
            }
        }
    });
    let (tx, rx) = mpsc::channel(1);
    tx.send(vec![1000; 1600]).await.unwrap();
    drop(tx);
    let result = fallback::transcribe_with_vocabulary(
        "fixture".into(),
        rx,
        endpoint,
        LocalEndpoint::new("http://127.0.0.1:1").unwrap(),
        Arc::new(AtomicU8::new(1)),
        Deadlines {
            connect: Duration::from_millis(500),
            cloud_final: Duration::from_secs(2),
            local: Duration::from_secs(1),
        },
        fallback::Progress::default(),
        &["C++".into(), "Project Acorn".into()],
    )
    .await
    .unwrap();
    assert_eq!(result, "Cloud fixture.");
    tokio::time::timeout(Duration::from_secs(3), server)
        .await
        .unwrap()
        .unwrap();
}
