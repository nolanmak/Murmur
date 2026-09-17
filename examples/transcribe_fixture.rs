//! Opt-in live smoke test. Sends a supplied mono 16 kHz signed-16-bit PCM fixture.
//! Never prints credentials. Run from the project directory to resolve .env.
use std::time::Duration;

#[tokio::main]
async fn main() -> Result<(), String> {
    let path = std::env::args().nth(1).ok_or("Supply a PCM fixture path")?;
    let bytes = std::fs::read(path).map_err(|_| "Cannot read fixture")?;
    if bytes.len() % 2 != 0 {
        return Err("Expected signed-16-bit PCM".into());
    }
    let credentials = text_to_speech::config::load()?;
    let (tx, rx) = tokio::sync::mpsc::channel(32);
    let producer = tokio::spawn(async move {
        for chunk in bytes.chunks(3200) {
            let pcm = chunk
                .chunks_exact(2)
                .map(|b| i16::from_le_bytes([b[0], b[1]]))
                .collect();
            if tx.send(pcm).await.is_err() {
                break;
            }
        }
    });
    let text = text_to_speech::streaming::transcribe(
        credentials.key.expose().into(),
        rx,
        fotw_stt::DeepgramEndpoint::production(),
        Duration::from_secs(8),
    )
    .await?;
    producer.await.map_err(|_| "Fixture sender failed")?;
    if text.is_empty() {
        return Err("No transcript returned".into());
    }
    println!("Fixture transcript: {text}");
    Ok(())
}
