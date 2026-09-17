//! The Deepgram streaming transport (spec 7.4, STT-03, STT-09).
//!
//! This is the socket the pure normalizer in [`crate::deepgram`] was written to
//! be fed by. It owns the four things a normalizer deliberately does not: the
//! WebSocket, the KeepAlive timer, the 30-second replay ring, and the reconnect
//! loop.
//!
//! Three §7.4 clauses drive most of the design:
//!
//! 1. **KeepAlive is a text frame, every 3–5 s.** Deepgram closes with 1011 /
//!    `NET-0001` after ten seconds with no audio, and a meeting has plenty of
//!    ten-second silences. The keepalive is suppressed while audio is flowing
//!    because audio is itself proof of life.
//! 2. **Audio is binary, control is text.** They are not interchangeable in
//!    either direction: JSON sent as binary is interpreted as PCM.
//! 3. **`diarize_model=v2` is batch-only.** Enforced in
//!    [`DeepgramStreamParams`](crate::deepgram_wire::DeepgramStreamParams)
//!    rather than here, so the failure is a config error at build time instead
//!    of a 400 at connect time.
//!
//! Reconnection is STT-09's, and its correctness rests on one invariant:
//! **only a final that closed an utterance normally moves `last_final_end_ms`.**
//!
//! When the socket dies mid-utterance, the partial in flight is finalized so
//! that spec 7.3's "every partial is eventually superseded" still holds. But
//! that synthesized final is not evidence that its audio was transcribed — the
//! provider never got to the end of it — so it must not move the replay anchor.
//! Replay therefore restarts from the last *complete* utterance, the provider
//! re-transcribes the one that was cut off, and the duplicate that produces is
//! removed by comparing normalized leading text against the transcript tail.
//! Letting the synthesized final move the anchor instead would skip the
//! replayed audio, and the words lost would never appear anywhere.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::http::header::{AUTHORIZATION, RETRY_AFTER};
use tokio_tungstenite::tungstenite::{Error as WsError, Message};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};

use crate::backoff::{BackoffPolicy, Jitter, ProcessJitter, ReconnectBudget};
use crate::dedupe::{DEFAULT_TAIL_TOKENS, TranscriptTail, trim_leading_tokens};
use crate::deepgram::{DeepgramConfig, DeepgramNormalizer, PROVIDER};
use crate::deepgram_wire::{
    CLOSE_STREAM_FRAME, DEFAULT_KEEPALIVE_MS, DeepgramEndpoint, DeepgramErrorFrame,
    DeepgramStreamParams, FINALIZE_FRAME, KEEPALIVE_FRAME, map_close, map_http_status,
};
use crate::replay::{PcmRing, to_linear16_le};
use crate::stream::{StreamEvent, StreamState};
use crate::{SttError, SttErrorClass, TranscriptSegment, UlidFactory};

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// Everything a [`DeepgramStream`] needs to open and keep a connection.
#[derive(Debug, Clone)]
pub struct DeepgramStreamConfig {
    /// Where to connect. Defaults to production; tests point it at a mock.
    pub endpoint: DeepgramEndpoint,
    /// The raw key, sent as `Authorization: Token <key>`.
    pub api_key: String,
    /// The §7.4 query parameters.
    pub params: DeepgramStreamParams,
    /// What the normalizer needs that the wire does not carry.
    pub normalizer: DeepgramConfig,
    /// KeepAlive cadence. §7.4's band is 3–5 s.
    pub keepalive: Duration,
    /// How much audio the replay ring keeps. STT-09 says 30 s.
    pub replay_window_ms: u64,
    /// The reconnect schedule and attempt budget.
    pub backoff: BackoffPolicy,
    /// How much committed transcript to keep for replay deduplication.
    pub tail_tokens: usize,
    /// Where this leg's own audio zero sits on the session clock (#86).
    ///
    /// A session has two capture legs and they do not start together, so a
    /// leg's provider clock — which counts the PCM this socket has swallowed
    /// and nothing else — is not the session clock. This is the constant
    /// between them, applied at connection zero and kept across every
    /// reconnect. Zero for the earlier leg, and zero for a single-leg session,
    /// where there is nothing to line up against.
    pub session_offset_ms: u64,
}

impl DeepgramStreamConfig {
    /// A production configuration for one capture stream.
    ///
    /// Diarization on the wire follows the normalizer's setting so the two
    /// cannot disagree — asking Deepgram to diarize while the normalizer is in
    /// forced-`me` mode is paying for labels that are then thrown away.
    #[must_use]
    pub fn new(api_key: impl Into<String>, normalizer: DeepgramConfig) -> Self {
        let params = DeepgramStreamParams::spec().with_diarize(normalizer.diarization_enabled);
        Self {
            endpoint: DeepgramEndpoint::production(),
            api_key: api_key.into(),
            params,
            normalizer,
            keepalive: Duration::from_millis(DEFAULT_KEEPALIVE_MS),
            replay_window_ms: crate::replay::DEFAULT_WINDOW_MS,
            backoff: BackoffPolicy::spec(),
            tail_tokens: DEFAULT_TAIL_TOKENS,
            session_offset_ms: 0,
        }
    }

    /// Anchor this leg `session_offset_ms` into the session (#86).
    #[must_use]
    pub fn with_session_offset_ms(mut self, session_offset_ms: u64) -> Self {
        self.session_offset_ms = session_offset_ms;
        self
    }

    /// Point this configuration at a local plaintext mock.
    #[must_use]
    pub fn with_endpoint(mut self, endpoint: DeepgramEndpoint) -> Self {
        self.endpoint = endpoint;
        self
    }

    /// Override the KeepAlive cadence.
    #[must_use]
    pub fn with_keepalive(mut self, keepalive: Duration) -> Self {
        self.keepalive = keepalive;
        self
    }

    /// Override the reconnect schedule.
    #[must_use]
    pub fn with_backoff(mut self, backoff: BackoffPolicy) -> Self {
        self.backoff = backoff;
        self
    }

    /// Override the §7.4 query parameters.
    #[must_use]
    pub fn with_params(mut self, params: DeepgramStreamParams) -> Self {
        self.params = params;
        self
    }

    /// Override the replay window.
    #[must_use]
    pub fn with_replay_window_ms(mut self, replay_window_ms: u64) -> Self {
        self.replay_window_ms = replay_window_ms;
        self
    }
}

/// A command from the application to the connection driver.
enum Command {
    Audio(Vec<i16>),
    Flush(oneshot::Sender<()>),
    Close(oneshot::Sender<()>),
}

/// A live Deepgram streaming connection.
///
/// The handle is cheap and `Send + Sync`; the socket lives on a spawned task.
/// Construction therefore requires a Tokio runtime to be entered.
#[derive(Debug)]
pub struct DeepgramStream {
    commands: mpsc::UnboundedSender<Command>,
    closed: Arc<AtomicBool>,
}

impl DeepgramStream {
    /// Open a stream and return it with its event channel.
    ///
    /// Returns immediately — the socket is established on the driver task, and
    /// the first events are `State(Connecting)` then either `State(Open)` or an
    /// `Error`. Audio written before the socket is up is buffered in the replay
    /// ring and sent on connect, so callers never have to wait for readiness.
    #[must_use]
    pub fn open(config: DeepgramStreamConfig) -> (Self, mpsc::UnboundedReceiver<StreamEvent>) {
        Self::open_with_jitter(config, Box::new(ProcessJitter::new()))
    }

    /// Open a stream with an injected jitter source, for reproducible tests.
    #[must_use]
    pub fn open_with_jitter(
        config: DeepgramStreamConfig,
        jitter: Box<dyn Jitter>,
    ) -> (Self, mpsc::UnboundedReceiver<StreamEvent>) {
        let (command_tx, command_rx) = mpsc::unbounded_channel();
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        let driver = Driver::new(config, event_tx, jitter);
        tokio::spawn(driver.run(command_rx));
        (
            Self {
                commands: command_tx,
                closed: Arc::new(AtomicBool::new(false)),
            },
            event_rx,
        )
    }

    /// Hand 16-bit little-endian mono PCM to the provider.
    ///
    /// Never blocks and never fails: after [`close`](Self::close), or if the
    /// driver has already given up, the samples are dropped. A recorder that
    /// stalls its audio pipeline because a transcription socket is unhappy has
    /// turned a degraded transcript into a lost recording.
    pub fn write(&self, pcm: &[i16]) {
        if pcm.is_empty() || self.closed.load(Ordering::SeqCst) {
            return;
        }
        let _ = self.commands.send(Command::Audio(pcm.to_vec()));
    }

    /// Force Deepgram to finalize what it has (`{"type":"Finalize"}`).
    ///
    /// Resolves once the frame is on the wire, not once the final arrives — the
    /// final comes back on the event channel like any other.
    pub async fn flush(&self) -> Result<(), SttError> {
        if self.closed.load(Ordering::SeqCst) {
            return Ok(());
        }
        let (ack_tx, ack_rx) = oneshot::channel();
        if self.commands.send(Command::Flush(ack_tx)).is_err() {
            return Err(driver_gone("flush"));
        }
        ack_rx.await.map_err(|_| driver_gone("flush"))
    }

    /// Close the stream. Idempotent, as spec 7.3's conformance suite requires.
    pub async fn close(&self) -> Result<(), SttError> {
        if self.closed.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        let (ack_tx, ack_rx) = oneshot::channel();
        if self.commands.send(Command::Close(ack_tx)).is_err() {
            // The driver already exited on its own, which is the same end state.
            return Ok(());
        }
        let _ = ack_rx.await;
        Ok(())
    }

    /// Whether [`close`](Self::close) has been called.
    #[must_use]
    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }
}

fn driver_gone(operation: &str) -> SttError {
    SttError::new(
        SttErrorClass::Network,
        PROVIDER,
        "the transcription stream is no longer running",
    )
    .with_detail(format!("{operation} was issued after the driver exited"))
}

/// Why a connection ended.
enum Outcome {
    /// The application asked to stop. Terminal.
    ClientClosed,
    /// The connection broke. Retryable-ness comes from the error's class.
    Failed(SttError),
}

struct Driver {
    config: DeepgramStreamConfig,
    events: mpsc::UnboundedSender<StreamEvent>,
    normalizer: DeepgramNormalizer<UlidFactory>,
    ring: PcmRing,
    tail: TranscriptTail,
    budget: ReconnectBudget,
    jitter: Box<dyn Jitter>,
    /// End of the last utterance Deepgram finalized normally, on the **session**
    /// clock. Where replay resumes from; see the module docs for why the
    /// dangling final must not move it, and [`Driver::leg_ms`] for the
    /// conversion the replay ring needs.
    last_final_end_ms: u64,
    /// Whether the next final is expected to restate replayed audio.
    dedupe_armed: bool,
    started: Instant,
    pending_close: Option<oneshot::Sender<()>>,
}

impl Driver {
    fn new(
        config: DeepgramStreamConfig,
        events: mpsc::UnboundedSender<StreamEvent>,
        jitter: Box<dyn Jitter>,
    ) -> Self {
        let normalizer =
            DeepgramNormalizer::anchored_at(config.normalizer.clone(), config.session_offset_ms);
        let ring = PcmRing::new(config.params.sample_rate, config.replay_window_ms);
        let tail = TranscriptTail::new(config.tail_tokens);
        let budget = config.backoff.budget();
        Self {
            config,
            events,
            normalizer,
            ring,
            tail,
            budget,
            jitter,
            last_final_end_ms: 0,
            dedupe_armed: false,
            started: Instant::now(),
            pending_close: None,
        }
    }

    async fn run(mut self, mut commands: mpsc::UnboundedReceiver<Command>) {
        self.emit(StreamEvent::State(StreamState::Connecting));

        let mut first_connection = true;
        loop {
            match self.connect().await {
                Ok(socket) => {
                    self.emit(StreamEvent::State(StreamState::Open));
                    match self.pump(socket, first_connection, &mut commands).await {
                        Outcome::ClientClosed => break,
                        Outcome::Failed(error) => {
                            let retryable = error.retryable;
                            self.emit(StreamEvent::Error(error));
                            if !retryable || !self.backoff_then_retry(&mut commands).await {
                                break;
                            }
                        }
                    }
                }
                Err(error) => {
                    let retryable = error.retryable;
                    self.emit(StreamEvent::Error(error));
                    if !retryable || !self.backoff_then_retry(&mut commands).await {
                        break;
                    }
                }
            }
            first_connection = false;
        }

        // Spec 7.3's conformance property: every partial is eventually
        // superseded by a final, including the one that was open when the
        // meeting ended.
        if let Some(dangling) = self.normalizer.finish() {
            self.commit_final(dangling);
        }
        self.emit(StreamEvent::State(StreamState::Closed));
        if let Some(ack) = self.pending_close.take() {
            let _ = ack.send(());
        }
    }

    async fn connect(&mut self) -> Result<Socket, SttError> {
        let url = self
            .config
            .endpoint
            .url_with(&self.config.params.to_query());

        let mut request = url.into_client_request().map_err(|error| {
            SttError::new(
                SttErrorClass::BadRequest,
                PROVIDER,
                "could not build the Deepgram request",
            )
            .with_detail(error.to_string())
        })?;

        let credential =
            HeaderValue::from_str(&format!("Token {}", self.config.api_key)).map_err(|error| {
                SttError::new(
                    SttErrorClass::Auth,
                    PROVIDER,
                    "the API key cannot be sent in an HTTP header",
                )
                .with_detail(error.to_string())
            })?;
        request.headers_mut().insert(AUTHORIZATION, credential);

        match connect_async(request).await {
            Ok((socket, _response)) => Ok(socket),
            Err(error) => Err(map_transport_error(&error)),
        }
    }

    async fn pump(
        &mut self,
        socket: Socket,
        first_connection: bool,
        commands: &mut mpsc::UnboundedReceiver<Command>,
    ) -> Outcome {
        let (mut sink, mut incoming) = socket.split();

        // Read the replay position *before* finalizing the dangling partial.
        // On this leg's own clock, because that is the one the ring counts:
        // it has been running since the leg's first sample, while
        // `last_final_end_ms` is on the session clock the anchor puts it on.
        let replay = self.ring.replay_from(self.leg_ms(self.last_final_end_ms));
        if !first_connection {
            if let Some(mut dangling) = self.normalizer.reconnected_at(replay.start_ms) {
                // The dangling partial goes through deduplication too. On a
                // single reconnect it never overlaps — the tail ends with the
                // previous utterance's final. But when a *second* socket dies
                // before producing any final, this partial restates the one the
                // *first* reconnect already committed, and nothing else in the
                // pipeline would catch it.
                let overlap = self.tail.overlap_with_text(&dangling.text);
                if overlap == 0 || trim_leading_tokens(&mut dangling, overlap) {
                    self.commit_final(dangling);
                }
            }
            self.dedupe_armed = true;
            if replay.lost_audio() {
                self.emit(StreamEvent::Error(
                    SttError::new(
                        SttErrorClass::Network,
                        PROVIDER,
                        "the outage outlived the replay buffer; some audio was not transcribed",
                    )
                    .with_detail(format!(
                        "{} ms had already been evicted from the {} ms replay ring",
                        replay.truncated_ms, self.config.replay_window_ms
                    )),
                ));
            }
        }
        if !replay.is_empty()
            && let Err(error) = sink
                .send(Message::binary(to_linear16_le(&replay.samples)))
                .await
        {
            return Outcome::Failed(map_transport_error(&error));
        }

        let mut keepalive = tokio::time::interval(self.config.keepalive);
        keepalive.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        // `interval` fires immediately on the first tick; the socket is brand
        // new, so that one is noise.
        keepalive.tick().await;
        let mut last_audio_at = Instant::now();

        loop {
            tokio::select! {
                command = commands.recv() => {
                    match command {
                        None => return Outcome::ClientClosed,
                        Some(Command::Audio(pcm)) => {
                            self.ring.push(&pcm);
                            if let Err(error) =
                                sink.send(Message::binary(to_linear16_le(&pcm))).await
                            {
                                return Outcome::Failed(map_transport_error(&error));
                            }
                            last_audio_at = Instant::now();
                        }
                        Some(Command::Flush(ack)) => {
                            let sent = sink.send(Message::text(FINALIZE_FRAME)).await;
                            let _ = ack.send(());
                            if let Err(error) = sent {
                                return Outcome::Failed(map_transport_error(&error));
                            }
                        }
                        Some(Command::Close(ack)) => {
                            self.pending_close = Some(ack);
                            let _ = sink.send(Message::text(CLOSE_STREAM_FRAME)).await;
                            let _ = sink.send(Message::Close(None)).await;
                            return Outcome::ClientClosed;
                        }
                    }
                }
                message = incoming.next() => {
                    match message {
                        Some(Ok(Message::Text(text))) => self.handle_text(text.as_str()),
                        Some(Ok(Message::Close(frame))) => {
                            let (code, reason) = match frame {
                                Some(frame) => (u16::from(frame.code), frame.reason.as_str().to_string()),
                                None => (1005, String::new()),
                            };
                            return Outcome::Failed(map_close(code, &reason).unwrap_or_else(|| {
                                // A clean close we did not ask for is the
                                // provider ending the session, not a fault.
                                SttError::new(
                                    SttErrorClass::SessionLimit,
                                    PROVIDER,
                                    "the provider ended the transcription session",
                                )
                            }));
                        }
                        Some(Ok(_)) => {}
                        Some(Err(error)) => return Outcome::Failed(map_transport_error(&error)),
                        None => {
                            return Outcome::Failed(
                                SttError::new(
                                    SttErrorClass::Network,
                                    PROVIDER,
                                    "the transcription socket ended without closing",
                                )
                                .with_detail("the peer went away without a close frame"),
                            );
                        }
                    }
                }
                _ = keepalive.tick() => {
                    // Audio is itself proof of life; a KeepAlive on top of it
                    // is wasted bandwidth on every second of every meeting.
                    if last_audio_at.elapsed() >= self.config.keepalive
                        && let Err(error) = sink.send(Message::text(KEEPALIVE_FRAME)).await
                    {
                        return Outcome::Failed(map_transport_error(&error));
                    }
                }
            }
        }
    }

    /// Wait out the backoff, returning `false` when the stream should stop.
    ///
    /// Audio that arrives during the wait still goes into the ring, which is
    /// what makes the replay *gapless* rather than merely *prompt*.
    async fn backoff_then_retry(
        &mut self,
        commands: &mut mpsc::UnboundedReceiver<Command>,
    ) -> bool {
        let now_ms = self.started.elapsed().as_millis() as u64;
        let Some(attempt) = self.budget.try_record(now_ms) else {
            self.emit(StreamEvent::Error(
                SttError::new(
                    SttErrorClass::Network,
                    PROVIDER,
                    "the transcription connection kept dropping; giving up on this provider",
                )
                .not_retryable(format!(
                    "{} reconnect attempts inside {} ms",
                    self.config.backoff.max_attempts, self.config.backoff.window_ms
                )),
            ));
            return false;
        };

        self.emit(StreamEvent::State(StreamState::Reconnecting));
        let delay =
            Duration::from_millis(self.config.backoff.delay_ms(attempt, self.jitter.unit()));
        self.drain_while_waiting(delay, commands).await
    }

    async fn drain_while_waiting(
        &mut self,
        delay: Duration,
        commands: &mut mpsc::UnboundedReceiver<Command>,
    ) -> bool {
        let deadline = tokio::time::Instant::now() + delay;
        loop {
            tokio::select! {
                _ = tokio::time::sleep_until(deadline) => return true,
                command = commands.recv() => {
                    match command {
                        None => return false,
                        Some(Command::Audio(pcm)) => {
                            self.ring.push(&pcm);
                        }
                        Some(Command::Flush(ack)) => {
                            // Nothing to finalize on a socket that is not up.
                            let _ = ack.send(());
                        }
                        Some(Command::Close(ack)) => {
                            self.pending_close = Some(ack);
                            return false;
                        }
                    }
                }
            }
        }
    }

    fn handle_text(&mut self, raw: &str) {
        if let Ok(frame) = serde_json::from_str::<DeepgramErrorFrame>(raw)
            && frame.is_error()
        {
            self.emit(StreamEvent::Error(frame.to_stt_error()));
            return;
        }

        // `push_json` classifies an unparseable frame as `server` and retryable:
        // the socket is fine, we just could not read one message, and taking the
        // meeting down over it would be a worse outcome than the missing words.
        match self.normalizer.push_json(raw) {
            Ok(Some(segment)) => self.emit_segment(segment),
            Ok(None) => {}
            Err(error) => self.emit(StreamEvent::Error(error)),
        }
    }

    /// Emit a segment, deduplicating the first final after a reconnect.
    fn emit_segment(&mut self, mut segment: TranscriptSegment) {
        if self.dedupe_armed {
            if !segment.is_final {
                // Partials over replayed audio restate text the user is already
                // looking at. Suppressing them avoids a visible rewind.
                return;
            }
            self.dedupe_armed = false;
            let overlap = self.tail.overlap_with_text(&segment.text);
            if overlap > 0 {
                let end_ms = segment.end_ms;
                if !trim_leading_tokens(&mut segment, overlap) {
                    // Wholly a replay. The audio is still accounted for, so the
                    // next replay must not start before it.
                    self.last_final_end_ms = self.last_final_end_ms.max(end_ms);
                    return;
                }
            }
        }

        if segment.is_final {
            self.last_final_end_ms = self.last_final_end_ms.max(segment.end_ms);
            self.commit_final(segment);
        } else {
            self.emit(StreamEvent::Partial(segment));
        }
    }

    /// A session-clock position on *this leg's* own audio clock (#86).
    ///
    /// The replay ring counts from this leg's first sample; the session clock
    /// counts from the earlier leg's. Everything the ring is asked about has to
    /// cross that constant, and this is the only place it does.
    fn leg_ms(&self, session_ms: u64) -> u64 {
        session_ms.saturating_sub(self.config.session_offset_ms)
    }

    /// Emit a final and remember its text for replay deduplication, without
    /// moving the replay anchor.
    fn commit_final(&mut self, segment: TranscriptSegment) {
        self.tail.push_text(&segment.text);
        self.emit(StreamEvent::Final(segment));
    }

    fn emit(&self, event: StreamEvent) {
        // The receiver being gone means the application stopped listening; the
        // driver still needs to run down its socket cleanly.
        let _ = self.events.send(event);
    }
}

/// Map a transport-level failure onto the shared taxonomy (STT-12).
#[must_use]
pub fn map_transport_error(error: &WsError) -> SttError {
    match error {
        WsError::Http(response) => {
            let status = response.status().as_u16();
            let retry_after = response
                .headers()
                .get(RETRY_AFTER)
                .and_then(|value| value.to_str().ok());
            let body = response
                .body()
                .as_ref()
                .map(|bytes| String::from_utf8_lossy(bytes).into_owned())
                .unwrap_or_default();
            map_http_status(status, retry_after, &body)
        }
        WsError::ConnectionClosed | WsError::AlreadyClosed => SttError::new(
            SttErrorClass::Network,
            PROVIDER,
            "the transcription socket is closed",
        ),
        WsError::Io(source) => SttError::new(
            SttErrorClass::Network,
            PROVIDER,
            SttErrorClass::Network.user_hint(),
        )
        .with_detail(source.to_string()),
        WsError::Tls(source) => SttError::new(
            SttErrorClass::Network,
            PROVIDER,
            "the TLS handshake with the transcription provider failed",
        )
        .with_detail(source.to_string()),
        // The abnormal-close case: the peer vanished without a close frame.
        // Transport, and retryable.
        WsError::Protocol(source) => SttError::new(
            SttErrorClass::Network,
            PROVIDER,
            "the transcription socket dropped",
        )
        .with_detail(source.to_string()),
        WsError::WriteBufferFull(_) => SttError::new(
            SttErrorClass::Network,
            PROVIDER,
            "the transcription socket could not keep up with the audio",
        ),
        WsError::Capacity(source) => SttError::new(
            SttErrorClass::BadRequest,
            PROVIDER,
            "a transcription frame exceeded the protocol limits",
        )
        .with_detail(source.to_string()),
        WsError::Url(source) => SttError::new(
            SttErrorClass::BadRequest,
            PROVIDER,
            "the transcription endpoint URL is invalid",
        )
        .with_detail(source.to_string()),
        WsError::HttpFormat(source) => SttError::new(
            SttErrorClass::BadRequest,
            PROVIDER,
            "the transcription handshake was malformed",
        )
        .with_detail(source.to_string()),
        WsError::Utf8(source) => SttError::new(
            SttErrorClass::Server,
            PROVIDER,
            "the provider sent a text frame that was not valid UTF-8",
        )
        .with_detail(source.clone()),
        WsError::AttackAttempt => SttError::new(
            SttErrorClass::Server,
            PROVIDER,
            "the transcription socket sent a frame that looked like an attack",
        ),
    }
}
