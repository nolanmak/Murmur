//! Loopback-only Whisper HTTP transport. Audio stays in memory; redirects and proxies are disabled.
use std::{
    sync::{
        Arc,
        atomic::{AtomicU8, Ordering},
    },
    time::Duration,
};
#[derive(Clone, Debug)]
pub struct LocalEndpoint {
    url: String,
    language: String,
    binding: Option<Capabilities>,
}
#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Capabilities {
    protocol_version: u8,
    instance_id: String,
    model_sha256: String,
    languages: Vec<String>,
}
impl Capabilities {
    fn validate(&self, language: &str) -> Result<(), String> {
        let valid_id = |value: &str| {
            value.len() == 64
                && value
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        };
        if self.protocol_version != 1
            || !valid_id(&self.instance_id)
            || !valid_id(&self.model_sha256)
            || self.languages.is_empty()
            || self.languages.len() > 128
            || self
                .languages
                .iter()
                .any(|code| crate::language::validate_tag(code).is_err())
            || self
                .languages
                .iter()
                .collect::<std::collections::HashSet<_>>()
                .len()
                != self.languages.len()
        {
            return Err("Invalid local language capability response".into());
        }
        if !self.languages.iter().any(|code| code == language) {
            return Err("Selected language is unsupported by the loaded local model; choose a supported language or load a compatible model".into());
        }
        Ok(())
    }
}
impl LocalEndpoint {
    /// This HTTP adapter does not encode recognition hints or provider prompts.
    pub fn vocabulary_capability(&self) -> fotw_stt::CustomVocabulary {
        fotw_stt::CustomVocabulary::None
    }

    pub fn language(&self) -> &str {
        &self.language
    }
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
        Ok(Self {
            url: url.to_string(),
            language: "en".into(),
            binding: None,
        })
    }
}
/// Verify the installed runtime has a loaded model before opening the microphone.
/// This is a loopback-only probe, never an automatic install/start/download.
pub async fn check_ready(endpoint: &LocalEndpoint, deadline: Duration) -> Result<(), String> {
    prepare(endpoint, "en", deadline).await.map(|_| ())
}

/// Snapshot the serving instance/model and bind inference to this preflight.
/// Legacy whisper.cpp exposes readiness only: it can support explicit English,
/// but must not be assumed to honor another language without model capabilities.
pub async fn prepare(
    endpoint: &LocalEndpoint,
    language: &str,
    deadline: Duration,
) -> Result<LocalEndpoint, String> {
    crate::language::validate_tag(language)?;
    let unavailable =
        "Local Whisper is not ready; install/load its model and start the engine before dictating";
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(deadline)
        .build()
        .map_err(|_| unavailable.to_string())?;
    let mut response = client
        .get(format!("{}health", endpoint.url))
        .send()
        .await
        .map_err(|_| unavailable.to_string())?;
    if !response.status().is_success() {
        return Err(unavailable.into());
    }
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| unavailable.to_string())?
    {
        if body.len() + chunk.len() > 16 * 1024 {
            return Err(unavailable.into());
        }
        body.extend_from_slice(&chunk);
    }
    let health: serde_json::Value =
        serde_json::from_slice(&body).map_err(|_| unavailable.to_string())?;
    if health["status"] != "ok" {
        return Err(unavailable.into());
    }
    let binding = match health.get("murmur_capabilities") {
        Some(value) => {
            let capabilities: Capabilities = serde_json::from_value(value.clone())
                .map_err(|_| "Invalid local language capability response")?;
            capabilities.validate(language)?;
            Some(capabilities)
        }
        None if language == "en" => None,
        None => return Err("Local engine cannot verify this language; its health API exposes no model capabilities. Use English or a capability-aware local runtime".into()),
    };
    Ok(LocalEndpoint {
        url: endpoint.url.clone(),
        language: language.into(),
        binding,
    })
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
            .text("response_format", "json")
            .text("language", endpoint.language.clone())
            .text("translate", "false")
            .text("detect_language", "false");
        let mut request = client
            .post(format!("{}inference", endpoint.url))
            .multipart(form);
        if let Some(binding) = &endpoint.binding {
            request = request
                .header("X-Murmur-Instance", &binding.instance_id)
                .header("X-Murmur-Model", &binding.model_sha256);
        }
        let mut response=request.send().await.map_err(|e|if e.is_timeout(){"Local inference timed out; no text inserted"}else{"Cannot reach local Whisper engine; start it with scripts/local-whisper.py --app murmur start"})?;
        if !response.status().is_success() {
            return Err(
                "Local Whisper rejected audio or is not ready; check model and engine status"
                    .to_string(),
            );
        }
        if let Some(binding) = &endpoint.binding
            && (response
                .headers()
                .get("X-Murmur-Instance")
                .and_then(|v| v.to_str().ok())
                != Some(binding.instance_id.as_str())
                || response
                    .headers()
                    .get("X-Murmur-Model")
                    .and_then(|v| v.to_str().ok())
                    != Some(binding.model_sha256.as_str()))
        {
            return Err("Local runtime/model changed after preflight; no text inserted".into());
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
