//! A scriptable stand-in for `wss://api.deepgram.com/v1/listen`.
//!
//! Binds `127.0.0.1:0`, reports its port, and records everything the client
//! sent — the request URI with its query string, the `Authorization` header, and
//! every text and binary frame. That recording is the point: assertions like
//! "KeepAlive went out as a *text* frame during the silence" and "`mip_opt_out`
//! was on the URL" can only be made from the server's side of the wire.
//!
//! Transcription is driven by the stamped PCM in [`super::pcm`], so a connection
//! that is re-fed audio it has already heard produces the same words again,
//! which is what makes the STT-09 replay path testable at all.

use std::collections::HashSet;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use fotw_stt::deepgram_wire::DeepgramEndpoint;
use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tokio_tungstenite::tungstenite::http;
use tokio_tungstenite::tungstenite::protocol::CloseFrame;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;

use super::pcm;
use super::script::{ScriptWord, TranscriptScript};

/// The `NET-0001` close Deepgram sends after ten seconds without audio or a
/// KeepAlive (spec 7.4).
pub const NET_0001_REASON: &str = "NET-0001 Deepgram did not receive audio data or a text message \
                                   within the timeout window.";

/// A `Results` frame truncated mid-object.
const MALFORMED_FRAME: &str = r#"{"type":"Results","channel":{"alternatives":[{"transcript":"#;

/// How a connection should misbehave.
#[derive(Debug, Clone, PartialEq)]
pub enum MockMode {
    /// Transcribe whatever audio arrives, per the script.
    Normal,
    /// Accept audio but send nothing back for this long, then behave normally.
    ///
    /// The client is expected to keep the socket alive through it.
    Stall(Duration),
    /// Vanish without a close handshake after this many client frames.
    DisconnectAfter(usize),
    /// Refuse the upgrade with HTTP 429 and a concurrency-flavoured body.
    Http429,
    /// Refuse the upgrade with HTTP 401.
    Auth401,
    /// Send one unparseable text frame, then carry on transcribing.
    MalformedJson,
    /// Close with WebSocket 1011 and a `NET-0001` reason.
    Close1011Net0001,
}

/// What one connection did, from the server's point of view.
#[derive(Debug, Clone, Default)]
pub struct ConnectionLog {
    /// The request target, including the query string.
    pub uri: String,
    /// The `Authorization` header, if the client sent one.
    pub authorization: Option<String>,
    /// Every header, lowercased names.
    pub headers: Vec<(String, String)>,
    /// Every text frame the client sent, in order.
    pub text_frames: Vec<String>,
    /// How many binary frames arrived.
    pub binary_frames: usize,
    /// How many 16-bit samples arrived in total.
    pub binary_samples: usize,
    /// The status this connection was refused with, if it was.
    pub refused_with: Option<u16>,
}

impl ConnectionLog {
    /// The query string, without the leading `?`.
    pub fn query(&self) -> &str {
        self.uri.split_once('?').map_or("", |(_, query)| query)
    }

    /// How many of the client's text frames were KeepAlives.
    pub fn keepalive_count(&self) -> usize {
        self.text_frames
            .iter()
            .filter(|frame| frame.contains("\"KeepAlive\""))
            .count()
    }

    /// Whether the client sent the given control frame type.
    pub fn sent_control(&self, message_type: &str) -> bool {
        self.text_frames
            .iter()
            .any(|frame| frame.contains(&format!("\"{message_type}\"")))
    }
}

/// Everything the mock observed, across every connection.
#[derive(Debug, Clone, Default)]
pub struct MockLog {
    /// One entry per accepted TCP connection, in order.
    pub connections: Vec<ConnectionLog>,
}

/// Builder for [`MockDeepgram`].
#[derive(Debug, Clone)]
pub struct MockDeepgramBuilder {
    default_mode: MockMode,
    connection_modes: Vec<MockMode>,
    script: TranscriptScript,
}

impl Default for MockDeepgramBuilder {
    fn default() -> Self {
        Self {
            default_mode: MockMode::Normal,
            connection_modes: Vec::new(),
            script: TranscriptScript::default(),
        }
    }
}

impl MockDeepgramBuilder {
    /// The mode every connection uses unless overridden per connection.
    pub fn mode(mut self, mode: MockMode) -> Self {
        self.default_mode = mode;
        self
    }

    /// Modes for connections 0, 1, 2 … Later connections fall back to the
    /// default mode, which is how a chaos run eventually terminates.
    pub fn connection_modes(mut self, modes: Vec<MockMode>) -> Self {
        self.connection_modes = modes;
        self
    }

    /// What the mock should transcribe.
    pub fn script(mut self, script: TranscriptScript) -> Self {
        self.script = script;
        self
    }

    /// Bind and start serving.
    pub async fn start(self) -> MockDeepgram {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("the mock provider could not bind a loopback port");
        let addr = listener
            .local_addr()
            .expect("the mock provider has no local address");
        let log = Arc::new(Mutex::new(MockLog::default()));
        let (shutdown_tx, shutdown_rx) = oneshot::channel();

        tokio::spawn(serve(
            listener,
            shutdown_rx,
            self.default_mode,
            self.connection_modes,
            Arc::new(self.script),
            log.clone(),
        ));

        MockDeepgram {
            addr,
            log,
            _shutdown: shutdown_tx,
        }
    }
}

/// A running mock provider. Dropping it stops the accept loop.
#[derive(Debug)]
pub struct MockDeepgram {
    addr: SocketAddr,
    log: Arc<Mutex<MockLog>>,
    _shutdown: oneshot::Sender<()>,
}

impl MockDeepgram {
    /// A builder.
    pub fn builder() -> MockDeepgramBuilder {
        MockDeepgramBuilder::default()
    }

    /// Start a normal mock with the given script.
    pub async fn start(script: TranscriptScript) -> Self {
        Self::builder().script(script).start().await
    }

    /// The bound port.
    pub fn port(&self) -> u16 {
        self.addr.port()
    }

    /// A plaintext endpoint pointing at this mock.
    pub fn endpoint(&self) -> DeepgramEndpoint {
        DeepgramEndpoint::loopback(self.addr.port())
    }

    /// A snapshot of everything observed so far.
    pub fn log(&self) -> MockLog {
        self.log.lock().expect("mock log poisoned").clone()
    }

    /// How many connections have been accepted.
    pub fn connection_count(&self) -> usize {
        self.log
            .lock()
            .expect("mock log poisoned")
            .connections
            .len()
    }

    /// A snapshot of one connection.
    pub fn connection(&self, index: usize) -> ConnectionLog {
        self.log
            .lock()
            .expect("mock log poisoned")
            .connections
            .get(index)
            .cloned()
            .unwrap_or_default()
    }
}

async fn serve(
    listener: TcpListener,
    mut shutdown: oneshot::Receiver<()>,
    default_mode: MockMode,
    connection_modes: Vec<MockMode>,
    script: Arc<TranscriptScript>,
    log: Arc<Mutex<MockLog>>,
) {
    loop {
        let accepted = tokio::select! {
            _ = &mut shutdown => return,
            accepted = listener.accept() => accepted,
        };
        let Ok((stream, _peer)) = accepted else {
            return;
        };

        // The slot is reserved before the handler is spawned so a connection's
        // index in the log is its arrival order, not its handshake order.
        let index = {
            let mut guard = log.lock().expect("mock log poisoned");
            guard.connections.push(ConnectionLog::default());
            guard.connections.len() - 1
        };
        let mode = connection_modes
            .get(index)
            .cloned()
            .unwrap_or_else(|| default_mode.clone());

        tokio::spawn(handle(stream, index, mode, script.clone(), log.clone()));
    }
}

// The handshake callback's `Result<Response, ErrorResponse>` is tungstenite's
// signature, not ours, and `ErrorResponse` cannot be boxed without breaking the
// `Callback` impl.
#[allow(clippy::result_large_err)]
async fn handle(
    stream: TcpStream,
    index: usize,
    mode: MockMode,
    script: Arc<TranscriptScript>,
    log: Arc<Mutex<MockLog>>,
) {
    let refusal = match mode {
        MockMode::Auth401 => Some((
            http::StatusCode::UNAUTHORIZED,
            r#"{"err_code":"INVALID_AUTH","err_msg":"Invalid credentials."}"#,
        )),
        MockMode::Http429 => Some((
            http::StatusCode::TOO_MANY_REQUESTS,
            r#"{"err_code":"TOO_MANY_REQUESTS","err_msg":"Maximum number of concurrent streams reached for this project."}"#,
        )),
        _ => None,
    };

    let capture = log.clone();
    let callback = move |request: &Request, response: Response| {
        {
            let mut guard = capture.lock().expect("mock log poisoned");
            let entry = &mut guard.connections[index];
            entry.uri = request.uri().to_string();
            for (name, value) in request.headers() {
                let value = value.to_str().unwrap_or_default().to_string();
                if name.as_str().eq_ignore_ascii_case("authorization") {
                    entry.authorization = Some(value.clone());
                }
                entry.headers.push((name.as_str().to_lowercase(), value));
            }
            entry.refused_with = refusal.map(|(status, _)| status.as_u16());
        }

        match refusal {
            Some((status, body)) => {
                let response: ErrorResponse = http::Response::builder()
                    .status(status)
                    .header(http::header::CONTENT_LENGTH, body.len())
                    .header(http::header::CONTENT_TYPE, "application/json")
                    .body(Some(body.to_string()))
                    .expect("the refusal response is well formed");
                Err(response)
            }
            None => Ok(response),
        }
    };

    let Ok(mut socket) = tokio_tungstenite::accept_hdr_async(stream, callback).await else {
        return;
    };

    let mut state = ConnectionState::default();
    let opened_at = Instant::now();
    let mut client_frames = 0usize;
    let mut sent_malformed = false;

    while let Some(message) = socket.next().await {
        let Ok(message) = message else { return };

        match message {
            Message::Text(text) => {
                client_frames += 1;
                log.lock().expect("mock log poisoned").connections[index]
                    .text_frames
                    .push(text.as_str().to_string());
            }
            Message::Binary(bytes) => {
                client_frames += 1;
                {
                    let mut guard = log.lock().expect("mock log poisoned");
                    let entry = &mut guard.connections[index];
                    entry.binary_frames += 1;
                    entry.binary_samples += bytes.len() / 2;
                }
                state.absorb(&bytes);
            }
            Message::Close(_) => return,
            _ => continue,
        }

        match &mode {
            MockMode::DisconnectAfter(limit) if client_frames >= *limit => {
                // Drop the socket without a close handshake: the client should
                // see an abnormal close, not a clean one.
                return;
            }
            MockMode::Close1011Net0001 if client_frames >= 1 => {
                let _ = socket
                    .send(Message::Close(Some(CloseFrame {
                        code: CloseCode::Error,
                        reason: NET_0001_REASON.into(),
                    })))
                    .await;
                let _ = socket.flush().await;
                return;
            }
            MockMode::Stall(duration) if opened_at.elapsed() < *duration => {}
            MockMode::MalformedJson if !sent_malformed => {
                sent_malformed = true;
                if socket.send(Message::text(MALFORMED_FRAME)).await.is_err() {
                    return;
                }
                if emit_ready_results(&mut socket, &script, &mut state)
                    .await
                    .is_err()
                {
                    return;
                }
            }
            _ => {
                if emit_ready_results(&mut socket, &script, &mut state)
                    .await
                    .is_err()
                {
                    return;
                }
            }
        }
    }
}

/// The audio one connection has been given, and what it has already said about
/// it.
#[derive(Debug, Default)]
struct ConnectionState {
    /// Samples left over from a frame that did not end on a millisecond.
    leftover: Vec<i16>,
    /// Session position of the first audio this connection received. Everything
    /// it reports is relative to this, exactly as a real connection's clock is.
    first_ms: Option<u64>,
    /// Session position of the most recent audio received.
    latest_ms: u64,
    /// Utterance indices whose interim has gone out on this connection.
    interim_sent: HashSet<usize>,
    /// Utterance indices already finalized on this connection.
    emitted: HashSet<usize>,
}

impl ConnectionState {
    fn absorb(&mut self, bytes: &[u8]) {
        self.leftover.extend(pcm::from_linear16_le(bytes));
        let whole = self.leftover.len() - self.leftover.len() % pcm::SAMPLES_PER_MS;
        let stamps = pcm::decode_stamps(&self.leftover[..whole]);
        self.leftover.drain(..whole);
        for stamp in stamps {
            self.first_ms.get_or_insert(stamp);
            self.latest_ms = self.latest_ms.max(stamp);
        }
    }

    /// Session position just past the audio received so far.
    fn received_through_ms(&self) -> u64 {
        if self.first_ms.is_none() {
            0
        } else {
            self.latest_ms + 1
        }
    }
}

async fn emit_ready_results(
    socket: &mut WebSocketStream<TcpStream>,
    script: &TranscriptScript,
    state: &mut ConnectionState,
) -> Result<(), ()> {
    let Some(first_ms) = state.first_ms else {
        return Ok(());
    };

    for (index, utterance) in script.utterances.iter().enumerate() {
        if state.emitted.contains(&index) {
            continue;
        }

        // Words whose audio predates this connection were never sent to it. A
        // mock that transcribed them anyway would be inventing text out of audio
        // it never received, which is the one thing a provider cannot do.
        let words: Vec<&ScriptWord> = utterance
            .words
            .iter()
            .filter(|word| word.start_ms >= first_ms)
            .collect();
        if words.is_empty() {
            state.emitted.insert(index);
            continue;
        }

        // The interim goes out as soon as the audio behind its half has
        // arrived, and the final only once the whole utterance has. **They must
        // not be emitted in the same breath**: the window between them is
        // exactly where a socket death leaves a dangling partial, which is the
        // case STT-09's deduplication exists for. A mock that sent both at once
        // would let a broken deduplicator pass.
        let half = words.len().div_ceil(2);
        if !state.interim_sent.contains(&index) {
            if words[half - 1].end_ms > state.received_through_ms() {
                // Chronological: a later utterance cannot be transcribed before
                // an earlier one whose audio has not arrived.
                break;
            }
            send_results(socket, &words[..half], first_ms, false, false).await?;
            state.interim_sent.insert(index);
        }

        if words[words.len() - 1].end_ms > state.received_through_ms() {
            break;
        }
        send_results(socket, &words, first_ms, true, true).await?;
        state.emitted.insert(index);
    }

    Ok(())
}

async fn send_results(
    socket: &mut WebSocketStream<TcpStream>,
    words: &[&ScriptWord],
    offset_ms: u64,
    is_final: bool,
    speech_final: bool,
) -> Result<(), ()> {
    let relative = |ms: u64| (ms.saturating_sub(offset_ms)) as f64 / 1000.0;
    let start = relative(words[0].start_ms);
    let end = relative(words[words.len() - 1].end_ms);

    let frame = json!({
        "type": "Results",
        "is_final": is_final,
        "speech_final": speech_final,
        "start": start,
        "duration": end - start,
        "channel": {
            "alternatives": [{
                "transcript": words
                    .iter()
                    .map(|word| word.text.as_str())
                    .collect::<Vec<_>>()
                    .join(" "),
                "confidence": 0.97,
                "words": words
                    .iter()
                    .map(|word| json!({
                        "word": word.text.to_lowercase(),
                        "punctuated_word": word.text,
                        "start": relative(word.start_ms),
                        "end": relative(word.end_ms),
                        "confidence": 0.96,
                        "speaker": word.speaker,
                    }))
                    .collect::<Vec<_>>(),
            }]
        }
    });

    socket
        .send(Message::text(frame.to_string()))
        .await
        .map_err(|_| ())
}
