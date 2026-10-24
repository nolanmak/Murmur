//! Synthetic fixture QA: cargo run --example local_transcribe -- URL mono16k.wav [LANGUAGE]
#[tokio::main]
async fn main() -> Result<(), String> {
    let args: Vec<String> = std::env::args().collect();
    if !(3..=4).contains(&args.len()) {
        return Err("Usage: local_transcribe http://127.0.0.1:17863 mono16k.wav [LANGUAGE]".into());
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
    let text = murmur::local_stt::transcribe(
        &endpoint,
        &pcm.ok_or("Missing audio data")?,
        std::sync::Arc::new(std::sync::atomic::AtomicU8::new(1)),
        std::time::Duration::from_secs(180),
    )
    .await?;
    println!("{text}");
    Ok(())
}
