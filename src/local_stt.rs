//! Loopback-only Whisper HTTP transport. Audio stays in memory; redirects and proxies are disabled.
use std::{
    sync::{
        Arc,
        atomic::{AtomicU8, Ordering},
    },
    time::Duration,
};
#[derive(Clone, Debug)]
pub struct LocalEndpoint(String);
impl LocalEndpoint {
    pub fn new(value: &str) -> Result<Self, String> {
        let url = reqwest::Url::parse(value).map_err(|_| "Invalid local engine URL")?;
        if url.scheme() != "http"
            || !matches!(url.host_str(), Some("127.0.0.1" | "[::1]"))
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || url.path() != "/"
        {
            return Err("Local engine must use http://127.0.0.1:PORT (or [::1]) without a path or credentials".into());
        }
        Ok(Self(url.to_string()))
    }
}
pub fn wav16k(pcm: &[i16]) -> Vec<u8> {
    let length = (pcm.len() * 2) as u32;
    let mut wav = Vec::with_capacity(44 + length as usize);
    wav.extend(b"RIFF");
    wav.extend((36 + length).to_le_bytes());
    wav.extend(b"WAVEfmt ");
    wav.extend(16u32.to_le_bytes());
    wav.extend(1u16.to_le_bytes());
    wav.extend(1u16.to_le_bytes());
    wav.extend(16000u32.to_le_bytes());
    wav.extend(32000u32.to_le_bytes());
    wav.extend(2u16.to_le_bytes());
    wav.extend(16u16.to_le_bytes());
    wav.extend(b"data");
    wav.extend(length.to_le_bytes());
    for sample in pcm {
        wav.extend(sample.to_le_bytes());
    }
    wav
}
pub async fn transcribe(
    endpoint: &LocalEndpoint,
    pcm: &[i16],
    control: Arc<AtomicU8>,
    deadline: Duration,
) -> Result<String, String> {
    if control.load(Ordering::Acquire) == 2 {
        return Err("Cancelled".into());
    }
    if pcm.len() > 16000 * 120 + 3200 {
        return Err("Local audio exceeds 120-second capture limit".into());
    }
    // Digital silence must never reach a generative recognizer that could hallucinate speech.
    if pcm.is_empty() || pcm.iter().all(|v| v.unsigned_abs() < 16) {
        return Ok(String::new());
    }
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(deadline)
        .build()
        .map_err(|_| "Cannot initialize local engine client")?;
    let request = async {
        let form = reqwest::multipart::Form::new()
            .part(
                "file",
                reqwest::multipart::Part::bytes(wav16k(pcm))
                    .file_name("audio.wav")
                    .mime_str("audio/wav")
                    .map_err(|_| "Invalid audio MIME type")?,
            )
            .text("response_format", "json");
        let mut response=client.post(format!("{}inference",endpoint.0)).multipart(form).send().await.map_err(|e|if e.is_timeout(){"Local inference timed out; no text inserted"}else{"Cannot reach local Whisper engine; start it with scripts/local-whisper.py --app murmur start"})?;
        if !response.status().is_success() {
            return Err(
                "Local Whisper rejected audio or is not ready; check model and engine status"
                    .to_string(),
            );
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| "Local engine response failed")?
        {
            if bytes.len() + chunk.len() > 1024 * 1024 {
                return Err("Local engine response too large".into());
            }
            bytes.extend(chunk);
        }
        let value: serde_json::Value =
            serde_json::from_slice(&bytes).map_err(|_| "Malformed local engine response")?;
        value
            .get("text")
            .and_then(|v| v.as_str())
            .map(|v| v.trim().to_owned())
            .ok_or_else(|| "Local engine response is missing transcript text".into())
    };
    tokio::pin!(request);
    let mut tick = tokio::time::interval(Duration::from_millis(10));
    loop {
        tokio::select! {result=&mut request=>{if control.load(Ordering::Acquire)==2{return Err("Cancelled".into());}return result;},_=tick.tick()=>{if control.load(Ordering::Acquire)==2{return Err("Cancelled".into());}}}
    }
}
