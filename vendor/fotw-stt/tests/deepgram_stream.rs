//! `DeepgramStream` against a local mock provider (spec 7.4, STT-03, STT-09,
//! STT-12).
//!
//! Every test here drives the real client over a real WebSocket to
//! `127.0.0.1`. Nothing reaches the internet and nothing needs a key, which is
//! the only way this code is exercised on CI at all.

mod support;

use std::time::Duration;

use fotw_stt::backoff::{BackoffPolicy, FixedJitter};
use fotw_stt::deepgram::{DeepgramConfig, PROVIDER};
use fotw_stt::deepgram_wire::DeepgramStreamParams;
use fotw_stt::{
    DeepgramStream, DeepgramStreamConfig, Source, StreamEvent, StreamState, SttErrorClass,
    TranscriptSegment,
};
use tokio::sync::mpsc::UnboundedReceiver;

use support::mock_deepgram::{MockDeepgram, MockMode};
use support::pcm::stamped_pcm;
use support::script::TranscriptScript;
use support::wait_until;

/// Generous enough that a loaded CI runner will not trip it, short enough that
/// a genuine hang fails the suite rather than the job timeout.
const TIMEOUT: Duration = Duration::from_secs(10);

fn config(mock: &MockDeepgram) -> DeepgramStreamConfig {
    DeepgramStreamConfig::new(
        "test-key-not-a-real-one",
        DeepgramConfig::new("session-1", Source::System),
    )
    .with_endpoint(mock.endpoint())
    .with_keepalive(Duration::from_millis(60))
    .with_backoff(BackoffPolicy::fast())
}

fn open(config: DeepgramStreamConfig) -> (DeepgramStream, UnboundedReceiver<StreamEvent>) {
    // A pinned jitter draw keeps the backoff schedule identical run to run.
    DeepgramStream::open_with_jitter(config, Box::new(FixedJitter(0.5)))
}

/// Collect events until `Closed`, or until the deadline.
async fn drain(events: &mut UnboundedReceiver<StreamEvent>) -> Vec<StreamEvent> {
    let mut collected = Vec::new();
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        match tokio::time::timeout(remaining, events.recv()).await {
            Ok(Some(event)) => {
                let terminal = event.state() == Some(StreamState::Closed);
                collected.push(event);
                if terminal {
                    return collected;
                }
            }
            Ok(None) | Err(_) => return collected,
        }
    }
}

fn finals(events: &[StreamEvent]) -> Vec<&TranscriptSegment> {
    events
        .iter()
        .filter_map(StreamEvent::final_segment)
        .collect()
}

fn states(events: &[StreamEvent]) -> Vec<StreamState> {
    events.iter().filter_map(StreamEvent::state).collect()
}

fn transcript(events: &[StreamEvent]) -> String {
    finals(events)
        .iter()
        .map(|segment| segment.text.as_str())
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// A three-utterance script: short enough to run fast, long enough for a
/// reconnect to land mid-utterance.
fn short_script() -> TranscriptScript {
    TranscriptScript::from_sentences(
        &[
            "alpha bravo charlie delta",
            "echo foxtrot golf hotel",
            "india juliet kilo lima",
        ],
        200,
        400,
    )
}

/// Feed the whole script in 100 ms writes.
fn write_script(stream: &DeepgramStream, script: &TranscriptScript) {
    let total = script.total_ms();
    let mut position = 0;
    while position < total {
        let span = 100.min(total - position);
        stream.write(&stamped_pcm(position, span));
        position += span;
    }
}

// ---------------------------------------------------------------------------
// The happy path
// ---------------------------------------------------------------------------

#[tokio::test]
async fn transcribes_a_stream_and_reports_its_lifecycle() {
    let script = short_script();
    let mock = MockDeepgram::start(script.clone()).await;
    let (stream, mut events) = open(config(&mock));

    write_script(&stream, &script);
    stream.flush().await.expect("flush reaches the socket");
    assert!(
        wait_until(TIMEOUT, || mock.connection(0).binary_frames > 0).await,
        "the mock never received audio"
    );

    // Wait for every scripted utterance to be finalized before closing.
    let mut collected = Vec::new();
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    while finals(&collected).len() < script.utterances.len()
        && tokio::time::Instant::now() < deadline
    {
        match tokio::time::timeout(Duration::from_millis(200), events.recv()).await {
            Ok(Some(event)) => collected.push(event),
            _ => break,
        }
    }
    stream.close().await.expect("close succeeds");
    collected.extend(drain(&mut events).await);

    assert_eq!(
        transcript(&collected),
        script.expected_text(),
        "the transcript must match the fixture word for word"
    );
    assert_eq!(
        states(&collected),
        vec![
            StreamState::Connecting,
            StreamState::Open,
            StreamState::Closed
        ],
        "an uninterrupted stream reports exactly three states"
    );

    // Spec 7.3's conformance property: no partial is left unsuperseded.
    for segment in finals(&collected) {
        assert!(segment.is_final);
        assert_eq!(segment.provider, PROVIDER);
        assert!(segment.end_ms >= segment.start_ms);
    }
}

#[tokio::test]
async fn the_request_carries_the_spec_url_and_the_token_header() {
    let mock = MockDeepgram::start(short_script()).await;
    let (stream, mut events) = open(config(&mock));
    stream.write(&stamped_pcm(0, 20));
    assert!(wait_until(TIMEOUT, || mock.connection(0).binary_frames > 0).await);

    let connection = mock.connection(0);
    assert_eq!(
        connection.authorization.as_deref(),
        Some("Token test-key-not-a-real-one"),
        "spec 7.4: Authorization: Token <key>"
    );
    assert!(connection.uri.starts_with("/v1/listen?"));

    let query = connection.query();
    for expected in [
        "model=nova-3",
        "encoding=linear16",
        "sample_rate=16000",
        "channels=1",
        "interim_results=true",
        "punctuate=true",
        "smart_format=true",
        "diarize=true",
        "endpointing=300",
        "utterance_end_ms=1000",
        "vad_events=true",
        "mip_opt_out=true",
    ] {
        assert!(query.contains(expected), "{expected} missing from {query}");
    }
    // Deepgram refuses the handshake outright when both are present:
    // "diarize_model cannot be used together with diarize or diarize_version."
    // It is an HTTP 400 on connect, so nothing is ever transcribed — and
    // because `session::run` consumes only `StreamEvent::Final`, the failure
    // is invisible and reads as "nobody spoke".
    assert!(
        !query.contains("diarize_model"),
        "diarize_model must not be sent alongside diarize: {query}"
    );

    stream.close().await.expect("close succeeds");
    drain(&mut events).await;
}

#[tokio::test]
async fn audio_goes_out_as_binary_frames_and_control_as_text() {
    let mock = MockDeepgram::start(short_script()).await;
    let (stream, mut events) = open(config(&mock));

    stream.write(&stamped_pcm(0, 50));
    stream.flush().await.expect("flush reaches the socket");
    assert!(
        wait_until(TIMEOUT, || {
            let connection = mock.connection(0);
            connection.binary_frames > 0 && connection.sent_control("Finalize")
        })
        .await,
        "expected one binary audio frame and one text Finalize"
    );

    let connection = mock.connection(0);
    assert_eq!(
        connection.binary_samples,
        50 * 16,
        "50 ms of 16 kHz mono arrived as binary, not as text"
    );
    assert!(
        connection
            .text_frames
            .iter()
            .all(|frame| frame.starts_with('{')),
        "every text frame is JSON control, never audio"
    );

    stream.close().await.expect("close succeeds");
    let events = drain(&mut events).await;
    assert!(!events.is_empty());

    // `flush()` is Finalize and `close()` is CloseStream: two different frames.
    // The client returning from `close()` only means the frames are on the
    // wire, so wait for the far end to have read them.
    assert!(
        wait_until(TIMEOUT, || {
            let connection = mock.connection(0);
            connection.sent_control("Finalize") && connection.sent_control("CloseStream")
        })
        .await,
        "expected both control frames, saw {:?}",
        mock.connection(0).text_frames
    );
}

// ---------------------------------------------------------------------------
// KeepAlive (§7.4's ten-second rule)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn keepalive_text_frames_go_out_during_silence() {
    // Without these the provider closes with 1011 / NET-0001 after ten seconds,
    // and a meeting is mostly silence.
    let mock = MockDeepgram::builder()
        .mode(MockMode::Stall(Duration::from_secs(30)))
        .script(short_script())
        .start()
        .await;
    let (stream, mut events) = open(config(&mock).with_keepalive(Duration::from_millis(40)));

    // One burst of audio, then nothing at all.
    stream.write(&stamped_pcm(0, 20));

    assert!(
        wait_until(TIMEOUT, || mock.connection(0).keepalive_count() >= 3).await,
        "expected repeated KeepAlives during the silence, saw {}",
        mock.connection(0).keepalive_count()
    );

    let connection = mock.connection(0);
    // The assertion that matters is that these arrived as *text*: the mock only
    // records a frame in `text_frames` when it was a text frame, so a KeepAlive
    // encoded as binary would land in `binary_frames` and never be counted.
    assert!(
        connection
            .text_frames
            .iter()
            .filter(|frame| frame.as_str() == r#"{"type":"KeepAlive"}"#)
            .count()
            >= 3
    );
    assert_eq!(
        connection.binary_frames, 1,
        "the KeepAlives must not have been sent as audio"
    );

    stream.close().await.expect("close succeeds");
    drain(&mut events).await;
}

#[tokio::test]
async fn keepalive_is_suppressed_while_audio_is_flowing() {
    // Audio is itself proof of life. A KeepAlive on top of it is wasted
    // bandwidth on every second of every meeting.
    let mock = MockDeepgram::builder()
        .mode(MockMode::Stall(Duration::from_secs(30)))
        .script(short_script())
        .start()
        .await;
    let (stream, mut events) = open(config(&mock).with_keepalive(Duration::from_millis(80)));

    let deadline = tokio::time::Instant::now() + Duration::from_millis(500);
    let mut position = 0;
    while tokio::time::Instant::now() < deadline {
        stream.write(&stamped_pcm(position, 20));
        position += 20;
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    assert_eq!(
        mock.connection(0).keepalive_count(),
        0,
        "no KeepAlive should be sent while audio keeps arriving"
    );
    assert!(mock.connection(0).binary_frames > 5);

    stream.close().await.expect("close succeeds");
    drain(&mut events).await;
}

// ---------------------------------------------------------------------------
// Failure classification (STT-12)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_401_is_an_auth_error_and_the_stream_does_not_retry() {
    let mock = MockDeepgram::builder()
        .mode(MockMode::Auth401)
        .start()
        .await;
    let (stream, mut events) = open(config(&mock));
    stream.write(&stamped_pcm(0, 20));

    let collected = drain(&mut events).await;
    let error = collected
        .iter()
        .find_map(StreamEvent::error)
        .expect("a 401 must surface as an error");

    assert_eq!(error.class, SttErrorClass::Auth);
    assert!(!error.retryable);
    assert_eq!(
        error.failover_policy(),
        fotw_stt::FailoverPolicy::Failover,
        "a rejected key is the one thing that should move to the next provider"
    );
    assert_eq!(
        states(&collected),
        vec![StreamState::Connecting, StreamState::Closed],
        "a bad key must not produce a reconnect loop"
    );
    assert_eq!(mock.connection_count(), 1, "no retry was attempted");
}

#[tokio::test]
async fn a_429_is_a_rate_limit_or_concurrency_error_and_is_retried() {
    let mock = MockDeepgram::builder()
        .mode(MockMode::Http429)
        .start()
        .await;
    let (stream, mut events) = open(config(&mock).with_backoff(BackoffPolicy {
        max_attempts: 2,
        ..BackoffPolicy::fast()
    }));
    stream.write(&stamped_pcm(0, 20));

    let collected = drain(&mut events).await;
    let error = collected
        .iter()
        .find_map(StreamEvent::error)
        .expect("a 429 must surface as an error");

    assert!(
        matches!(
            error.class,
            SttErrorClass::RateLimit | SttErrorClass::Concurrency
        ),
        "429 is rate_limit or concurrency, got {:?}",
        error.class
    );
    assert!(error.retryable);
    assert_eq!(error.failover_policy(), fotw_stt::FailoverPolicy::Backoff);
    assert!(
        states(&collected).contains(&StreamState::Reconnecting),
        "pressure is backed off, not failed over"
    );
}

#[tokio::test]
async fn a_1011_net_0001_close_is_a_network_error_and_reconnects() {
    // The §7.4 silence timeout. Only the first connection closes this way; the
    // second is normal, which is what a real recovery looks like.
    let script = short_script();
    let mock = MockDeepgram::builder()
        .connection_modes(vec![MockMode::Close1011Net0001])
        .script(script.clone())
        .start()
        .await;
    let (stream, mut events) = open(config(&mock));

    write_script(&stream, &script);
    assert!(
        wait_until(TIMEOUT, || mock.connection_count() >= 2).await,
        "the stream never reconnected"
    );

    let mut collected = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    while tokio::time::Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_millis(200), events.recv()).await {
            Ok(Some(event)) => collected.push(event),
            _ => break,
        }
    }
    stream.close().await.expect("close succeeds");
    collected.extend(drain(&mut events).await);

    let error = collected
        .iter()
        .find_map(StreamEvent::error)
        .expect("the close must surface");
    assert_eq!(error.class, SttErrorClass::Network);
    assert!(error.retryable);
    assert!(error.message.contains("NET-0001"));

    let states = states(&collected);
    assert_eq!(states[0], StreamState::Connecting);
    assert!(states.contains(&StreamState::Reconnecting));
    assert_eq!(states.last(), Some(&StreamState::Closed));
    assert!(mock.connection_count() >= 2);
}

#[tokio::test]
async fn a_malformed_frame_is_a_retryable_server_error_and_the_socket_survives() {
    // One unreadable frame must not cost the user the rest of their meeting.
    let script = short_script();
    let mock = MockDeepgram::builder()
        .mode(MockMode::MalformedJson)
        .script(script.clone())
        .start()
        .await;
    let (stream, mut events) = open(config(&mock));

    write_script(&stream, &script);

    let mut collected = Vec::new();
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    while finals(&collected).len() < script.utterances.len()
        && tokio::time::Instant::now() < deadline
    {
        match tokio::time::timeout(Duration::from_millis(200), events.recv()).await {
            Ok(Some(event)) => collected.push(event),
            _ => break,
        }
    }
    stream.close().await.expect("close succeeds");
    collected.extend(drain(&mut events).await);

    let error = collected
        .iter()
        .find_map(StreamEvent::error)
        .expect("the malformed frame must surface as an error");
    assert_eq!(error.class, SttErrorClass::Server);
    assert!(error.retryable, "the socket is fine; one message was not");

    assert_eq!(
        transcript(&collected),
        script.expected_text(),
        "the rest of the transcript survived the bad frame"
    );
    assert_eq!(
        mock.connection_count(),
        1,
        "a bad frame must not trigger a reconnect"
    );
}

#[tokio::test]
async fn an_abnormal_disconnect_reconnects_and_finishes_the_transcript() {
    let script = short_script();
    let mock = MockDeepgram::builder()
        // Vanish after the third client frame, i.e. part way through the audio.
        .connection_modes(vec![MockMode::DisconnectAfter(3)])
        .script(script.clone())
        .start()
        .await;
    let (stream, mut events) = open(config(&mock));

    write_script(&stream, &script);

    let mut collected = Vec::new();
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    while finals(&collected).len() < script.utterances.len()
        && tokio::time::Instant::now() < deadline
    {
        match tokio::time::timeout(Duration::from_millis(200), events.recv()).await {
            Ok(Some(event)) => collected.push(event),
            _ => break,
        }
    }
    stream.close().await.expect("close succeeds");
    collected.extend(drain(&mut events).await);

    assert!(
        mock.connection_count() >= 2,
        "the stream must have reconnected"
    );
    assert_eq!(
        transcript(&collected),
        script.expected_text(),
        "gapless replay must recover every word the outage covered"
    );

    // Timestamps stay on the session clock across the reconnect (spec 7.2
    // rule 1): the second connection's zero is not session zero.
    let finals = finals(&collected);
    let last = finals.last().expect("at least one final");
    assert!(
        last.end_ms >= script.total_ms().saturating_sub(50),
        "the last final should land near the end of the fixture, got {}ms",
        last.end_ms
    );
    let mut previous_start = 0;
    for segment in &finals {
        assert!(
            segment.start_ms >= previous_start,
            "finals must not go backwards across a reconnect"
        );
        previous_start = segment.start_ms;
    }
}

#[tokio::test]
async fn a_legs_session_anchor_survives_a_reconnect() {
    // #86's piece three, through the real driver and the real replay ring.
    //
    // The mic leg's audio starts after the system leg's, so its connection is
    // opened with a constant anchor: everything it reports is that much later
    // on the session clock. `reconnected_at` recomputes the *connection*
    // offset from the replay ring, which counts from this leg's own first
    // sample — so the anchor has to survive underneath it. If it did not, one
    // dropped socket would drag the mic leg's timeline back onto the system
    // leg's, and the transcript would look fine while being wrong. That is
    // exactly the class of failure #79 was.
    const ANCHOR_MS: u64 = 7_000;

    let script = short_script();
    let mock = MockDeepgram::builder()
        // Vanish after the third client frame, i.e. part way through the audio.
        .connection_modes(vec![MockMode::DisconnectAfter(3)])
        .script(script.clone())
        .start()
        .await;
    let (stream, mut events) = open(config(&mock).with_session_offset_ms(ANCHOR_MS));

    write_script(&stream, &script);

    let mut collected = Vec::new();
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    while finals(&collected).len() < script.utterances.len()
        && tokio::time::Instant::now() < deadline
    {
        match tokio::time::timeout(Duration::from_millis(200), events.recv()).await {
            Ok(Some(event)) => collected.push(event),
            _ => break,
        }
    }
    stream.close().await.expect("close succeeds");
    collected.extend(drain(&mut events).await);

    assert!(
        mock.connection_count() >= 2,
        "the stream must have reconnected for this to prove anything"
    );
    assert_eq!(
        transcript(&collected),
        script.expected_text(),
        "anchoring must not disturb gapless replay"
    );

    let finals = finals(&collected);
    for segment in &finals {
        assert!(
            segment.start_ms >= ANCHOR_MS,
            "a segment landed at {} ms, before this leg's audio exists",
            segment.start_ms
        );
    }
    let last = finals.last().expect("at least one final");
    assert!(
        last.end_ms >= ANCHOR_MS + script.total_ms().saturating_sub(50),
        "the last final should land near {} ms — the anchor plus the fixture — \
         got {} ms",
        ANCHOR_MS + script.total_ms(),
        last.end_ms
    );
}

#[tokio::test]
async fn the_stream_gives_up_once_the_attempt_budget_is_spent() {
    // Every connection dies immediately, so the budget is the only thing that
    // ends the loop.
    let mock = MockDeepgram::builder()
        .mode(MockMode::DisconnectAfter(1))
        .script(short_script())
        .start()
        .await;
    let (stream, mut events) = open(config(&mock).with_backoff(BackoffPolicy {
        max_attempts: 3,
        ..BackoffPolicy::fast()
    }));

    stream.write(&stamped_pcm(0, 100));
    let collected = drain(&mut events).await;

    assert_eq!(
        states(&collected).last(),
        Some(&StreamState::Closed),
        "the stream must terminate rather than reconnect forever"
    );
    assert!(
        mock.connection_count() <= 4,
        "one initial connection plus at most three retries, saw {}",
        mock.connection_count()
    );
    let last_error = collected
        .iter()
        .filter_map(StreamEvent::error)
        .next_back()
        .expect("giving up must be reported");
    assert!(!last_error.retryable, "the final error is terminal");
}

// ---------------------------------------------------------------------------
// Lifecycle
// ---------------------------------------------------------------------------

#[tokio::test]
async fn close_is_idempotent() {
    // Spec 7.3's conformance suite requires it.
    let mock = MockDeepgram::start(short_script()).await;
    let (stream, mut events) = open(config(&mock));
    stream.write(&stamped_pcm(0, 20));

    stream.close().await.expect("first close");
    stream.close().await.expect("second close");
    stream.close().await.expect("third close");
    assert!(stream.is_closed());

    // Writes after close are dropped rather than panicking.
    stream.write(&stamped_pcm(20, 20));

    let collected = drain(&mut events).await;
    assert_eq!(
        collected
            .iter()
            .filter(|event| event.state() == Some(StreamState::Closed))
            .count(),
        1,
        "exactly one Closed state, however many times close() was called"
    );
}

#[tokio::test]
async fn audio_written_before_the_socket_opens_is_not_lost() {
    // The ring is the buffer: `write` is synchronous and returns before the
    // handshake has finished, so anything spoken in the first moments of a
    // meeting would otherwise be dropped on the floor.
    let script = short_script();
    let mock = MockDeepgram::start(script.clone()).await;
    let mut configuration = config(&mock);
    configuration.normalizer = DeepgramConfig::new("session-1", Source::System);
    let (stream, mut events) = open(configuration);

    write_script(&stream, &script);

    let mut collected = Vec::new();
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    while finals(&collected).len() < script.utterances.len()
        && tokio::time::Instant::now() < deadline
    {
        match tokio::time::timeout(Duration::from_millis(200), events.recv()).await {
            Ok(Some(event)) => collected.push(event),
            _ => break,
        }
    }
    stream.close().await.expect("close succeeds");
    collected.extend(drain(&mut events).await);

    assert_eq!(transcript(&collected), script.expected_text());
}

#[tokio::test]
async fn the_mic_stream_is_labelled_me_and_never_diarized() {
    // Spec 7.2 rule 2 plus 7.5: the mic is one known person, so diarizing it is
    // a paid add-on that can only introduce error.
    let script = short_script();
    let mock = MockDeepgram::start(script.clone()).await;
    let configuration =
        DeepgramStreamConfig::new("test-key", DeepgramConfig::new("session-mic", Source::Mic))
            .with_endpoint(mock.endpoint())
            .with_keepalive(Duration::from_millis(60))
            .with_backoff(BackoffPolicy::fast());

    assert!(!configuration.params.to_query().contains("diarize=true"));

    let (stream, mut events) = open(configuration);
    write_script(&stream, &script);

    let mut collected = Vec::new();
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    while finals(&collected).is_empty() && tokio::time::Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_millis(200), events.recv()).await {
            Ok(Some(event)) => collected.push(event),
            _ => break,
        }
    }
    stream.close().await.expect("close succeeds");
    collected.extend(drain(&mut events).await);

    let finals = finals(&collected);
    assert!(!finals.is_empty());
    for segment in finals {
        assert_eq!(segment.speaker.as_deref(), Some("me"));
        assert_eq!(segment.source, Source::Mic);
    }
}

#[tokio::test]
async fn a_stream_whose_parameters_are_rejected_never_opens_a_socket() {
    // `diarize_model=v2` is caught before the connection, not by a 400 from the
    // provider after the meeting has already started.
    let error = DeepgramStreamParams::spec()
        .with_diarize_model("v2")
        .expect_err("v2 is batch-only");
    assert_eq!(error.class, SttErrorClass::Unsupported);
}
