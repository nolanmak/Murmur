//! Fixture QA: local_transcribe [--simulate-cloud-loss | --cancel-after-ms N] URL mono16k.wav [LANGUAGE].
//! The optional fake cloud is owned loopback only; no credentials are resolved.
#[tokio::main]
async fn main() -> Result<(), String> {
    let mut args: Vec<String> = std::env::args().collect();
    let cancel_ms = if args.get(1).is_some_and(|v| v == "--cancel-after-ms") {
        let value = args
            .get(2)
            .and_then(|s| s.parse::<u64>().ok())
            .filter(|v| (1..=5000).contains(v))
            .ok_or("Cancellation delay must be 1..=5000 milliseconds")?;
        args.drain(1..3);
        Some(value)
    } else {
        None
    };
    let simulate_loss = args.get(1).is_some_and(|v| v == "--simulate-cloud-loss");
    if simulate_loss {
        args.remove(1);
    }
    if !(3..=4).contains(&args.len()) {
        return Err("Usage: local_transcribe [--simulate-cloud-loss | --cancel-after-ms N] http://127.0.0.1:17863 mono16k.wav [LANGUAGE]".into());
    }
    let wav = std::fs::read(&args[2]).map_err(|e| e.to_string())?;
    if wav.len() < 44 || &wav[..4] != b"RIFF" || &wav[8..12] != b"WAVE" {
        return Err("Expected PCM WAV".into());
    }
    let mut cursor = 12;
    let mut pcm = None;
    let mut format = false;
    while cursor + 8 <= wav.len() {
        let length = u32::from_le_bytes(wav[cursor + 4..cursor + 8].try_into().unwrap()) as usize;
        let start = cursor + 8;
        if start + length > wav.len() {
            return Err("Truncated WAV".into());
        }
        match &wav[cursor..cursor + 4] {
            b"fmt " if length >= 16 => {
                format = wav[start..start + 2] == 1u16.to_le_bytes()
                    && wav[start + 2..start + 4] == 1u16.to_le_bytes()
                    && wav[start + 4..start + 8] == 16000u32.to_le_bytes()
                    && wav[start + 14..start + 16] == 16u16.to_le_bytes();
            }
            b"data" => {
                pcm = Some(
                    wav[start..start + length]
                        .chunks_exact(2)
                        .map(|b| i16::from_le_bytes([b[0], b[1]]))
                        .collect::<Vec<_>>(),
                )
            }
            _ => {}
        }
        cursor = start + length + (length % 2);
    }
    if !format {
        return Err("Expected mono 16kHz signed PCM16".into());
    }
    let endpoint = murmur::local_stt::prepare(
        &murmur::local_stt::LocalEndpoint::new(&args[1])?,
        args.get(3).map(String::as_str).unwrap_or("en"),
        std::time::Duration::from_secs(2),
    )
    .await?;
    let pcm = pcm.ok_or("Missing audio data")?;
    if let Some(ms) = cancel_ms {
        if simulate_loss {
            return Err("Choose one fixture mode".into());
        }
        let report = cancel_after(&endpoint, &pcm, std::time::Duration::from_millis(ms)).await?;
        eprintln!("fixture_cancellation={report}");
        return Ok(());
    }
    let text = if simulate_loss {
        recover_after_loss(endpoint, pcm).await?
    } else {
        murmur::local_stt::transcribe(
            &endpoint,
            &pcm,
            std::sync::Arc::new(std::sync::atomic::AtomicU8::new(1)),
            std::time::Duration::from_secs(180),
        )
        .await?
    };
    println!("{text}");
    Ok(())
}

/// Cancel the real adapter after a fixture delay, never print its transcript.
/// Completion before the signal is an invalid measurement, not successful cancellation.
async fn cancel_after(
    endpoint: &murmur::local_stt::LocalEndpoint,
    pcm: &[i16],
    delay: std::time::Duration,
) -> Result<serde_json::Value, String> {
    cancel_when(endpoint, pcm, tokio::time::sleep(delay)).await
}

/// Let the synthetic server acknowledge the entire request before its test timer starts.
/// The CLI keeps its original timer-at-adapter-start behavior through cancel_after.
async fn cancel_when(
    endpoint: &murmur::local_stt::LocalEndpoint,
    pcm: &[i16],
    signal: impl std::future::Future<Output = ()>,
) -> Result<serde_json::Value, String> {
    use std::sync::{
        Arc,
        atomic::{AtomicU8, Ordering},
    };
    use std::time::Duration;
    let control = Arc::new(AtomicU8::new(1));
    let began = std::time::Instant::now();
    let inference =
        murmur::local_stt::transcribe(endpoint, pcm, control.clone(), Duration::from_secs(180));
    tokio::pin!(inference);
    tokio::select! {
        _=&mut inference=>return Err("Inference completed before fixture cancellation; no cancellation was measured".into()),
        _=signal=>{}
    }
    control.store(2, Ordering::Release);
    let cancelled = std::time::Instant::now();
    let result = tokio::time::timeout(Duration::from_secs(2), inference)
        .await
        .map_err(|_| "Adapter did not return within the cancellation fixture deadline")?;
    if !matches!(result,Err(ref message) if message=="Cancelled") {
        return Err("Adapter did not acknowledge cancellation; output withheld".into());
    }
    Ok(
        serde_json::json!({"cancelled":true,"transcript_returned":false,
        "input_samples":pcm.len(),"cancel_requested_after_seconds":cancelled.duration_since(began).as_secs_f64(),
        "cancel_to_return_seconds":cancelled.elapsed().as_secs_f64(),
        "trigger":"fixture timer, not native Escape","compute_interruption_measured":false,
        "insertion_performed":false,"real_provider_calls":0}),
    )
}

async fn recover_after_loss(
    endpoint: murmur::local_stt::LocalEndpoint,
    pcm: Vec<i16>,
) -> Result<String, String> {
    use futures_util::{SinkExt, StreamExt};
    use std::sync::{
        Arc,
        atomic::{AtomicU8, AtomicUsize, Ordering},
    };
    use std::time::Duration;
    use tokio_tungstenite::{accept_async, tungstenite::Message};
    if pcm.len() < 2 {
        return Err("Cloud-loss fixture needs nonempty input before and after failure".into());
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|e| e.to_string())?;
    let cloud = fotw_stt::DeepgramEndpoint::loopback(
        listener.local_addr().map_err(|e| e.to_string())?.port(),
    );
    let control = Arc::new(AtomicU8::new(0));
    let producer_control = control.clone();
    let connections = Arc::new(AtomicUsize::new(0));
    let seen = connections.clone();
    let heard = Arc::new(AtomicUsize::new(0));
    let captured = heard.clone();
    let total = pcm.len();
    let split = total / 2;
    let (tx, rx) = tokio::sync::mpsc::channel(8);
    tx.send(pcm[..split].to_vec())
        .await
        .map_err(|_| "Fixture input channel closed")?;
    let tail = pcm[split..].to_vec();
    let server = tokio::spawn(async move {
        let mut sender = Some(tx);
        for attempt in 0..2 {
            let accepted = tokio::time::timeout(Duration::from_secs(4), listener.accept()).await;
            let Ok(Ok((socket, _))) = accepted else { break };
            seen.fetch_add(1, Ordering::AcqRel);
            let Ok(mut ws) = accept_async(socket).await else {
                break;
            };
            if attempt == 0 {
                while let Some(Ok(message)) = ws.next().await {
                    if let Message::Binary(bytes) = message {
                        captured.store(bytes.len() / 2, Ordering::Release);
                        break;
                    }
                }
                ws.send(Message::text(r#"{"type":"Results","is_final":true,"speech_final":true,"channel":{"alternatives":[{"transcript":"Incomplete cloud fixture text."}]}}"#)).await.map_err(|e|e.to_string())?;
            }
            let _ = ws.close(None).await;
            if let Some(tx) = sender.take() {
                tokio::time::sleep(Duration::from_millis(30)).await;
                tx.send(tail.clone()).await.map_err(|_| {
                    "Capture must accept remaining audio after the disconnect".to_string()
                })?;
                producer_control.store(1, Ordering::Release);
            }
        }
        Ok::<(), String>(())
    });
    let progress = murmur::fallback::Progress::default();
    let profile = murmur::vocabulary::Profile {
        language: endpoint.language().to_owned(),
        ..Default::default()
    };
    let result = tokio::time::timeout(
        Duration::from_secs(190),
        murmur::fallback::transcribe_with_profile(
            "public-fixture-not-a-key".into(),
            rx,
            cloud,
            endpoint,
            control,
            murmur::fallback::Deadlines::default(),
            progress.clone(),
            &profile,
        ),
    )
    .await;
    server.abort();
    let _ = server.await;
    let text = result.map_err(|_| "Cloud-loss fixture deadline exceeded")??;
    if heard.load(Ordering::Acquire) != split
        || !matches!(progress.stage(), murmur::fallback::Stage::Local)
        || text.contains("Incomplete cloud fixture text")
    {
        return Err("Whole-utterance local fallback was not established".into());
    }
    eprintln!(
        "fixture_recovery={}",
        serde_json::json!({"input_samples":total,"cloud_samples_before_disconnect":heard.load(Ordering::Acquire),"additional_samples_after_disconnect":total-split,"cloud_connections":connections.load(Ordering::Acquire),"result_provider":progress.stage().label(),"partial_cloud_text_discarded":true,"real_provider_calls":0,"insertion_performed":false})
    );
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn loss_fixture_preserves_selected_language_and_entire_input() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = murmur::local_stt::LocalEndpoint::new(&format!(
            "http://{}",
            listener.local_addr().unwrap()
        ))
        .unwrap();
        let id = "a".repeat(64);
        let server = tokio::spawn(async move {
            let mut inference = Vec::new();
            for health in [true, false] {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                loop {
                    let mut buf = [0; 8192];
                    let n = socket.read(&mut buf).await.unwrap();
                    assert_ne!(n, 0);
                    request.extend_from_slice(&buf[..n]);
                    if let Some(end) = request.windows(4).position(|s| s == b"\r\n\r\n") {
                        let headers = String::from_utf8_lossy(&request[..end]).to_lowercase();
                        let len = headers
                            .lines()
                            .find_map(|s| s.strip_prefix("content-length: "))
                            .map(|v| v.parse::<usize>().unwrap())
                            .unwrap_or(0);
                        if request.len() >= end + 4 + len {
                            break;
                        }
                    }
                }
                let body = if health {
                    serde_json::json!({"status":"ok","murmur_capabilities":{"protocol_version":1,"instance_id":id,"model_sha256":id,"languages":["es"]}}).to_string()
                } else {
                    inference = request;
                    serde_json::json!({"text":"Local fixture complete."}).to_string()
                };
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nX-Murmur-Instance: {id}\r\nX-Murmur-Model: {id}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                socket.write_all(response.as_bytes()).await.unwrap();
            }
            inference
        });
        let endpoint =
            murmur::local_stt::prepare(&endpoint, "es", std::time::Duration::from_secs(2))
                .await
                .unwrap();
        let pcm: Vec<i16> = (0..32000)
            .map(|i| ((i * 17) % 30001 - 15000) as i16)
            .collect();
        let text = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            recover_after_loss(endpoint, pcm.clone()),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(text, "Local fixture complete.");
        let request = server.await.unwrap();
        let wav = murmur::local_stt::wav16k(&pcm);
        assert!(request.windows(wav.len()).any(|v| v == wav));
        assert!(String::from_utf8_lossy(&request).contains("name=\"language\"\r\n\r\nes\r\n"));
    }
    #[tokio::test]
    async fn cancellation_fixture_suppresses_a_late_engine_response() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = murmur::local_stt::LocalEndpoint::new(&format!(
            "http://{}",
            listener.local_addr().unwrap()
        ))
        .unwrap();
        let received = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let observed = received.clone();
        let (received_tx, received_rx) = tokio::sync::oneshot::channel();
        let (late_tx, late_rx) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            let mut request = Vec::new();
            loop {
                let mut bytes = [0; 8192];
                let n = socket.read(&mut bytes).await.unwrap();
                assert_ne!(n, 0);
                request.extend_from_slice(&bytes[..n]);
                if let Some(end) = request.windows(4).position(|v| v == b"\r\n\r\n") {
                    let headers = String::from_utf8_lossy(&request[..end]).to_lowercase();
                    let len = headers
                        .lines()
                        .find_map(|s| s.strip_prefix("content-length: "))
                        .unwrap()
                        .parse::<usize>()
                        .unwrap();
                    if request.len() >= end + 4 + len {
                        break;
                    }
                }
            }
            observed.store(true, std::sync::atomic::Ordering::Release);
            received_tx.send(()).unwrap();
            // Release the late response only after cancellation is acknowledged.
            // This also avoids a scheduling race between two elapsed timers.
            late_rx.await.unwrap();
            let body = r#"{"text":"Late fixture text must not be delivered"}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = socket.write_all(response.as_bytes()).await;
            request
        });
        let pcm = vec![1000; 1600];
        let report = cancel_when(&endpoint, &pcm, async {
            tokio::time::timeout(std::time::Duration::from_secs(2), received_rx)
                .await
                .unwrap()
                .unwrap();
            tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        })
        .await
        .unwrap();
        assert!(
            received.load(std::sync::atomic::Ordering::Acquire),
            "cancellation timing must start after the complete request is received"
        );
        assert_eq!(report["cancelled"], true);
        assert_eq!(report["transcript_returned"], false);
        assert!(report["cancel_to_return_seconds"].as_f64().unwrap() < 2.0);
        late_tx.send(()).unwrap();
        let request = server.await.unwrap();
        let wav = murmur::local_stt::wav16k(&pcm);
        assert!(request.windows(wav.len()).any(|s| s == wav));
    }

    #[tokio::test]
    async fn an_already_completed_request_is_not_claimed_as_cancellation() {
        let endpoint = murmur::local_stt::LocalEndpoint::new("http://127.0.0.1:1").unwrap();
        let error = cancel_after(&endpoint, &[0; 1600], std::time::Duration::from_millis(30))
            .await
            .unwrap_err();
        assert!(error.contains("no cancellation was measured"));
    }
}
