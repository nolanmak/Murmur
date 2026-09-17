//! The Deepgram streaming wire contract: URL, query parameters, control frames
//! and failure mapping (spec 7.4, STT-03, STT-12).
//!
//! Pure and synchronous, with no socket and no runtime, so every clause of §7.4
//! that is easy to get wrong is assertable in a plain unit test:
//!
//! * `mip_opt_out=true` is on **every** request, not just the ones where a
//!   setting is enabled. It is the only thing standing between a customer's
//!   meeting audio and Deepgram's model-improvement program (spec 10), and a
//!   parameter that is conditional is a parameter that will one day be missing.
//! * `diarize_model=v2` is batch-only and fails validation on a stream, so the
//!   streaming path sends `v1` and refuses to be configured otherwise.
//! * KeepAlive is a **text** frame. Sending the same JSON as binary is not a
//!   KeepAlive, it is two bytes of audio, and the socket dies at the ten-second
//!   mark exactly as if nothing had been sent.

use serde::{Deserialize, Serialize};

use crate::{SttError, SttErrorClass, deepgram::PROVIDER};

/// The production streaming endpoint.
pub const PRODUCTION_HOST: &str = "api.deepgram.com";
/// The streaming path.
pub const LISTEN_PATH: &str = "/v1/listen";

/// The KeepAlive control frame. **Must be sent as a text frame** (spec 7.4).
pub const KEEPALIVE_FRAME: &str = r#"{"type":"KeepAlive"}"#;
/// The frame that forces the current audio to finalize — `SttStream::flush`.
pub const FINALIZE_FRAME: &str = r#"{"type":"Finalize"}"#;
/// The frame that asks Deepgram to finish and close cleanly.
pub const CLOSE_STREAM_FRAME: &str = r#"{"type":"CloseStream"}"#;

/// The longest silence Deepgram tolerates before closing with 1011 / NET-0001.
pub const SILENCE_TIMEOUT_MS: u64 = 10_000;
/// The KeepAlive cadence: the middle of §7.4's 3–5 s band.
pub const DEFAULT_KEEPALIVE_MS: u64 = 4_000;

/// The diarization model the streaming API accepts.
pub const STREAMING_DIARIZE_MODEL: &str = "v1";
/// The diarization model that only works on pre-recorded audio.
pub const BATCH_ONLY_DIARIZE_MODEL: &str = "v2";

// ---------------------------------------------------------------------------
// Endpoint
// ---------------------------------------------------------------------------

/// Where to open the socket.
///
/// Injectable so the test suite can point a real client at a local mock over
/// plain `ws://`. That is the whole reason this type exists: the alternative is
/// a client that can only be exercised against Deepgram itself, which means it
/// is exercised on a CI runner with no secrets exactly never.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeepgramEndpoint {
    /// `wss` in production, `ws` against a local mock.
    pub scheme: String,
    /// Host, without port.
    pub host: String,
    /// Port, or `None` for the scheme default.
    pub port: Option<u16>,
    /// Path, e.g. `/v1/listen`.
    pub path: String,
}

impl Default for DeepgramEndpoint {
    fn default() -> Self {
        Self::production()
    }
}

impl DeepgramEndpoint {
    /// `wss://api.deepgram.com/v1/listen`.
    #[must_use]
    pub fn production() -> Self {
        Self {
            scheme: "wss".to_string(),
            host: PRODUCTION_HOST.to_string(),
            port: None,
            path: LISTEN_PATH.to_string(),
        }
    }

    /// A plaintext endpoint at `host:port`, for a local mock.
    ///
    /// Never reachable from the production config: nothing in the app builds one
    /// of these, so a plaintext socket cannot appear by configuration accident.
    #[must_use]
    pub fn insecure(host: impl Into<String>, port: u16) -> Self {
        Self {
            scheme: "ws".to_string(),
            host: host.into(),
            port: Some(port),
            path: LISTEN_PATH.to_string(),
        }
    }

    /// A local mock listening on `127.0.0.1:port`.
    #[must_use]
    pub fn loopback(port: u16) -> Self {
        Self::insecure("127.0.0.1", port)
    }

    /// The full URL with `query` appended.
    #[must_use]
    pub fn url_with(&self, query: &str) -> String {
        let authority = match self.port {
            Some(port) => format!("{}:{}", self.host, port),
            None => self.host.clone(),
        };
        if query.is_empty() {
            format!("{}://{}{}", self.scheme, authority, self.path)
        } else {
            format!("{}://{}{}?{}", self.scheme, authority, self.path, query)
        }
    }

    /// Whether this endpoint is encrypted.
    #[must_use]
    pub fn is_secure(&self) -> bool {
        self.scheme.eq_ignore_ascii_case("wss")
    }
}

// ---------------------------------------------------------------------------
// Query parameters
// ---------------------------------------------------------------------------

/// The streaming query parameters of spec 7.4.
///
/// `diarize_model` is private with a validating setter, because it is the one
/// field where a plausible value silently breaks the connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeepgramStreamParams {
    /// `model`, e.g. `nova-3`.
    pub model: String,
    /// `encoding`. `linear16` is the only thing our pipeline produces.
    pub encoding: String,
    /// `sample_rate`, in Hz.
    pub sample_rate: u32,
    /// `channels`. Always 1 — the two-stream default (spec 7.5) means each
    /// socket carries exactly one capture stream.
    pub channels: u16,
    /// `interim_results`.
    pub interim_results: bool,
    /// `punctuate`.
    pub punctuate: bool,
    /// `smart_format`.
    pub smart_format: bool,
    /// `diarize`. Off for the mic stream, which is one known person.
    pub diarize: bool,
    /// `endpointing`, in milliseconds.
    pub endpointing_ms: u32,
    /// `utterance_end_ms`.
    pub utterance_end_ms: u32,
    /// `vad_events`.
    pub vad_events: bool,
    /// `language`, when pinned rather than detected.
    pub language: Option<String>,
    /// Repeated `keyterm=` values (STT-14).
    pub keyterms: Vec<String>,
    diarize_model: String,
}

impl Default for DeepgramStreamParams {
    fn default() -> Self {
        Self::spec()
    }
}

impl DeepgramStreamParams {
    /// Exactly the parameter set §7.4 lists.
    #[must_use]
    pub fn spec() -> Self {
        Self {
            model: crate::deepgram::DEFAULT_MODEL.to_string(),
            encoding: "linear16".to_string(),
            sample_rate: crate::replay::DEFAULT_SAMPLE_RATE,
            channels: 1,
            interim_results: true,
            punctuate: true,
            smart_format: true,
            diarize: true,
            endpointing_ms: 300,
            utterance_end_ms: 1_000,
            vad_events: true,
            language: None,
            keyterms: Vec::new(),
            diarize_model: STREAMING_DIARIZE_MODEL.to_string(),
        }
    }

    /// The diarization model this request will send.
    #[must_use]
    pub fn diarize_model(&self) -> &str {
        &self.diarize_model
    }

    /// Set the diarization model, rejecting the batch-only one.
    ///
    /// `v2` is not a better `v1`; on a stream it is a 400 that takes the whole
    /// connection down before a single word is transcribed. Catching it here
    /// turns a runtime outage into a config error with a `Surface` policy.
    pub fn with_diarize_model(mut self, model: impl Into<String>) -> Result<Self, SttError> {
        let model = model.into();
        if model.eq_ignore_ascii_case(BATCH_ONLY_DIARIZE_MODEL) {
            return Err(SttError::new(
                SttErrorClass::Unsupported,
                PROVIDER,
                "diarize_model=v2 is pre-recorded only; streaming requires v1",
            )
            .with_detail(
                "spec 7.4: Deepgram returns a validation error for diarize_model=v2 on \
                 /v1/listen streaming connections",
            ));
        }
        self.diarize_model = model;
        Ok(self)
    }

    /// Turn diarization on or off.
    #[must_use]
    pub fn with_diarize(mut self, diarize: bool) -> Self {
        self.diarize = diarize;
        self
    }

    /// Replace the keyterm list (STT-14).
    #[must_use]
    pub fn with_keyterms<I, S>(mut self, keyterms: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.keyterms = keyterms.into_iter().map(Into::into).collect();
        self
    }

    /// Pin the language instead of detecting it.
    #[must_use]
    pub fn with_language(mut self, language: impl Into<String>) -> Self {
        self.language = Some(language.into());
        self
    }

    /// The query string, without a leading `?`.
    ///
    /// Order follows §7.4 so a captured URL diffs cleanly against the spec.
    #[must_use]
    pub fn to_query(&self) -> String {
        let mut pairs: Vec<(String, String)> = vec![
            ("model".into(), self.model.clone()),
            ("encoding".into(), self.encoding.clone()),
            ("sample_rate".into(), self.sample_rate.to_string()),
            ("channels".into(), self.channels.to_string()),
            ("interim_results".into(), bool_param(self.interim_results)),
            ("punctuate".into(), bool_param(self.punctuate)),
            ("smart_format".into(), bool_param(self.smart_format)),
        ];

        // `diarize` alone, never with `diarize_model`.
        //
        // Deepgram refuses the handshake when both are present —
        // "diarize_model cannot be used together with diarize or
        // diarize_version" — as an HTTP 400 on connect, before a single word
        // is transcribed. Sending both was a silent, total loss of
        // transcription: `session::run` consumes only `StreamEvent::Final`, so
        // the error went nowhere and an empty `stt.jsonl` read as a meeting
        // where nobody spoke.
        //
        // The earlier comment here reasoned that `diarize_model` alone would
        // select a model for a feature that is still off. That is true, and
        // the conclusion drawn from it — send both — is what broke it. The
        // parameter is kept on the struct because `with_diarize_model` still
        // guards the batch-only `v2`, and a caller that sets it explicitly
        // should still be told no.
        if self.diarize {
            pairs.push(("diarize".into(), "true".into()));
        }

        pairs.push(("endpointing".into(), self.endpointing_ms.to_string()));
        pairs.push(("utterance_end_ms".into(), self.utterance_end_ms.to_string()));
        pairs.push(("vad_events".into(), bool_param(self.vad_events)));

        if let Some(language) = &self.language {
            pairs.push(("language".into(), language.clone()));
        }

        // Unconditional. See the module docs.
        pairs.push(("mip_opt_out".into(), "true".into()));

        for keyterm in &self.keyterms {
            pairs.push(("keyterm".into(), keyterm.clone()));
        }

        pairs
            .into_iter()
            .map(|(key, value)| format!("{}={}", percent_encode(&key), percent_encode(&value)))
            .collect::<Vec<_>>()
            .join("&")
    }
}

fn bool_param(value: bool) -> String {
    if value { "true".into() } else { "false".into() }
}

/// Percent-encode everything outside RFC 3986's unreserved set.
///
/// Hand-rolled rather than a `percent-encoding` dependency: the alphabet is
/// four lines and the crate would be the only thing it is used for.
#[must_use]
pub fn percent_encode(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(*byte as char);
            }
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
}

// ---------------------------------------------------------------------------
// Failure mapping (STT-12)
// ---------------------------------------------------------------------------

/// A `{"type":"Error"}` frame delivered on the socket rather than as a close.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeepgramErrorFrame {
    /// The frame type. Only `Error` is one of these.
    #[serde(rename = "type", default, skip_serializing_if = "Option::is_none")]
    pub message_type: Option<String>,
    /// Deepgram's human-readable description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// A short message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// The `XXX-NNNN` code, when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub err_code: Option<String>,
    /// The variant tag Deepgram sometimes uses instead of `err_code`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub variant: Option<String>,
}

impl DeepgramErrorFrame {
    /// Whether this really is an error frame.
    #[must_use]
    pub fn is_error(&self) -> bool {
        self.message_type.as_deref() == Some("Error")
    }

    /// Normalize it into the shared taxonomy.
    #[must_use]
    pub fn to_stt_error(&self) -> SttError {
        let reason = [
            self.err_code.as_deref(),
            self.variant.as_deref(),
            self.message.as_deref(),
            self.description.as_deref(),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" ");

        let class = class_for_code(extract_deepgram_code(&reason).as_deref())
            .unwrap_or(SttErrorClass::Server);
        let message = self
            .description
            .clone()
            .or_else(|| self.message.clone())
            .unwrap_or_else(|| class.user_hint().to_string());
        SttError::new(class, PROVIDER, message)
            .with_detail(format!("deepgram error frame: {reason}"))
    }
}

/// Pull a `XXX-NNNN` Deepgram code out of free text.
#[must_use]
pub fn extract_deepgram_code(reason: &str) -> Option<String> {
    reason
        .split(|character: char| character.is_whitespace() || matches!(character, ',' | ';' | '"'))
        .map(|token| token.trim_matches(|c: char| matches!(c, '.' | ':' | ')' | '(' | '\'')))
        .find(|token| is_deepgram_code(token))
        .map(str::to_string)
}

fn is_deepgram_code(token: &str) -> bool {
    let Some((prefix, digits)) = token.split_once('-') else {
        return false;
    };
    !prefix.is_empty()
        && prefix.len() <= 8
        && prefix.chars().all(|c| c.is_ascii_uppercase())
        && digits.len() == 4
        && digits.chars().all(|c| c.is_ascii_digit())
}

fn class_for_code(code: Option<&str>) -> Option<SttErrorClass> {
    let code = code?;
    let prefix = code.split_once('-')?.0;
    match prefix {
        // NET-0001 is the ten-second-silence close. Transport, and retryable:
        // the fix is a new socket, not a different provider.
        "NET" => Some(SttErrorClass::Network),
        // DATA-0000 is "we could not decode what you sent".
        "DATA" => Some(SttErrorClass::AudioFormat),
        _ => None,
    }
}

/// Map an HTTP handshake failure onto the taxonomy (STT-12).
///
/// `retry_after` is the raw header value; only the integer-seconds form is
/// parsed, because the HTTP-date form has never been observed from Deepgram and
/// guessing at a date parse would be more code than the case is worth.
#[must_use]
pub fn map_http_status(status: u16, retry_after: Option<&str>, body: &str) -> SttError {
    let lowercase = body.to_ascii_lowercase();
    let class = match status {
        400 if mentions_audio_format(&lowercase) => SttErrorClass::AudioFormat,
        400 => SttErrorClass::BadRequest,
        401 | 403 => SttErrorClass::Auth,
        402 => SttErrorClass::Quota,
        // Deepgram's concurrency ceiling is per *project*, not per key (spec
        // 7.5), and the two-stream default consumes two of them. It arrives as
        // a 429 like an ordinary rate limit but is a different fact, so the
        // supervisor can degrade to single mixed mono rather than just waiting.
        429 if mentions_concurrency(&lowercase) => SttErrorClass::Concurrency,
        429 => SttErrorClass::RateLimit,
        404 | 405 | 409 | 413 | 415 | 422 => SttErrorClass::BadRequest,
        500..=599 => SttErrorClass::Server,
        _ => SttErrorClass::Server,
    };

    let message = first_meaningful_line(body).unwrap_or_else(|| class.user_hint().to_string());
    let mut error = SttError::new(class, PROVIDER, message)
        .with_detail(format!("deepgram handshake returned HTTP {status}"));
    if let Some(seconds) = retry_after.and_then(parse_retry_after_seconds) {
        error = error.with_retry_after_ms(seconds.saturating_mul(1_000));
    }
    error
}

fn mentions_concurrency(body: &str) -> bool {
    body.contains("concurren") || body.contains("simultaneous")
}

fn mentions_audio_format(body: &str) -> bool {
    body.contains("encoding")
        || body.contains("sample_rate")
        || body.contains("sample rate")
        || body.contains("channels")
}

fn parse_retry_after_seconds(value: &str) -> Option<u64> {
    value.trim().parse::<u64>().ok()
}

fn first_meaningful_line(body: &str) -> Option<String> {
    // Deepgram's error bodies are JSON like {"err_code":..,"err_msg":..}. Prefer
    // the message field when it parses, and fall back to the raw body so a
    // shape change downgrades the log line rather than losing it.
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(body) {
        for key in ["err_msg", "message", "description", "error", "reason"] {
            if let Some(text) = value.get(key).and_then(serde_json::Value::as_str)
                && !text.trim().is_empty()
            {
                return Some(text.trim().to_string());
            }
        }
    }
    let trimmed = body.trim();
    (!trimmed.is_empty()).then(|| trimmed.chars().take(400).collect())
}

/// Map a WebSocket close onto the taxonomy, or `None` if it was clean.
///
/// §7.4's named case — 1011 with `NET-0001` after ten seconds of silence — has
/// to come out `network` and retryable, because the correct response is a new
/// socket. Classifying it `server` would still retry, but it would also count
/// toward a demotion budget for a fault that was ours: we stopped sending
/// KeepAlives.
#[must_use]
pub fn map_close(code: u16, reason: &str) -> Option<SttError> {
    if let Some(deepgram_code) = extract_deepgram_code(reason)
        && let Some(class) = class_for_code(Some(&deepgram_code))
    {
        return Some(close_error(class, code, reason));
    }

    let class = match code {
        1000 | 1001 => return None,
        1002 | 1003 | 1007 | 1008 | 1009 | 1010 => SttErrorClass::BadRequest,
        1011 => SttErrorClass::Server,
        1012 | 1006 | 1005 => SttErrorClass::Network,
        1013 => SttErrorClass::RateLimit,
        4000..=4999 => SttErrorClass::Server,
        _ => SttErrorClass::Network,
    };
    Some(close_error(class, code, reason))
}

fn close_error(class: SttErrorClass, code: u16, reason: &str) -> SttError {
    let message = if reason.trim().is_empty() {
        class.user_hint().to_string()
    } else {
        reason.trim().to_string()
    };
    SttError::new(class, PROVIDER, message)
        .with_detail(format!("deepgram closed the socket with {code}: {reason}"))
}
