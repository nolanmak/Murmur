//! Fixture QA: local_transcribe [--simulate-cloud-loss] URL mono16k.wav [LANGUAGE].
//! The optional fake cloud is owned loopback only; no credentials are resolved.
#[tokio::main]
async fn main() -> Result<(), String> {
    let mut args: Vec<String> = std::env::args().collect();
    let simulate_loss = args.get(1).is_some_and(|v| v == "--simulate-cloud-loss");
    if simulate_loss {
        args.remove(1);
    }
    if !(3..=4).contains(&args.len()) {
        return Err("Usage: local_transcribe [--simulate-cloud-loss] http://127.0.0.1:17863 mono16k.wav [LANGUAGE]".into());
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
}
