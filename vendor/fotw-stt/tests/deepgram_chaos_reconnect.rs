//! STT-09's acceptance test: kill the socket at random points and check the
//! transcript.
//!
//! > *Acceptance: chaos test kills the socket at random points in a 30-min
//! > fixture, WER delta < 1%.*
//!
//! The fixture here is 50 seconds rather than 30 minutes — the mock transcribes
//! as fast as audio arrives, so the length that matters is the number of
//! reconnects and where they land, not wall-clock duration. Five kills at
//! seeded-random points cover both cases that behave differently: a socket that
//! dies mid-utterance, leaving a dangling partial that the replay will restate,
//! and one that dies in the silence between utterances, where there is nothing
//! to deduplicate and a deduplicator that trims anyway would eat real words.
//!
//! The comparison is against an uninterrupted run of the same fixture rather
//! than against the fixture text, so the test measures what reconnection cost
//! and not what the mock happens to say.

mod support;

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use fotw_stt::backoff::BackoffPolicy;
use fotw_stt::deepgram::DeepgramConfig;
use fotw_stt::{
    DeepgramStream, DeepgramStreamConfig, FixedJitter, Source, StreamEvent, StreamState,
};

use support::mock_deepgram::{MockDeepgram, MockMode};
use support::pcm::stamped_pcm;
use support::script::TranscriptScript;
use support::wer::word_error_rate;
use support::{Lcg, wait_until};

/// Audio handed over per write.
const CHUNK_MS: u64 = 100;

/// How much untranscribed audio the writer lets accumulate.
///
/// Well inside the 30 s ring, so the ring is never the limiting factor and the
/// test measures deduplication rather than eviction. Eviction has its own test
/// in `deepgram_transport_units.rs`.
const IN_FLIGHT_MS: u64 = 6_000;

const RUN_TIMEOUT: Duration = Duration::from_secs(60);

struct RunResult {
    transcript: String,
    connections: usize,
    reconnects: usize,
}

async fn run(script: &TranscriptScript, modes: Vec<MockMode>) -> RunResult {
    let mock = MockDeepgram::builder()
        .connection_modes(modes)
        .script(script.clone())
        .start()
        .await;

    let configuration = DeepgramStreamConfig::new(
        "chaos-test-key",
        DeepgramConfig::new("chaos-session", Source::System),
    )
    .with_endpoint(mock.endpoint())
    .with_keepalive(Duration::from_millis(250))
    .with_backoff(BackoffPolicy::fast());

    let (stream, mut events) =
        DeepgramStream::open_with_jitter(configuration, Box::new(FixedJitter(0.5)));

    let progress = Arc::new(AtomicU64::new(0));
    let finished = Arc::new(AtomicBool::new(false));
    let reconnects = Arc::new(AtomicU64::new(0));
    let transcript = Arc::new(Mutex::new(Vec::<String>::new()));

    let collector = tokio::spawn({
        let progress = progress.clone();
        let finished = finished.clone();
        let reconnects = reconnects.clone();
        let transcript = transcript.clone();
        async move {
            while let Some(event) = events.recv().await {
                match event {
                    StreamEvent::Final(segment) => {
                        progress.fetch_max(segment.end_ms, Ordering::SeqCst);
                        if !segment.text.trim().is_empty() {
                            transcript
                                .lock()
                                .expect("transcript poisoned")
                                .push(segment.text);
                        }
                    }
                    StreamEvent::State(StreamState::Reconnecting) => {
                        reconnects.fetch_add(1, Ordering::SeqCst);
                    }
                    StreamEvent::State(StreamState::Closed) => {
                        finished.store(true, Ordering::SeqCst);
                        break;
                    }
                    _ => {}
                }
            }
        }
    });

    // Feed the fixture at a bounded lead over what has been transcribed, the way
    // a real capture pipeline does. Writing the whole meeting into the channel
    // at once would let the ring evict audio an outage still needed.
    let total = script.total_ms();
    let mut position = 0;
    let deadline = tokio::time::Instant::now() + RUN_TIMEOUT;
    while position < total {
        let span = CHUNK_MS.min(total - position);
        stream.write(&stamped_pcm(position, span));
        position += span;

        while position.saturating_sub(progress.load(Ordering::SeqCst)) > IN_FLIGHT_MS {
            if tokio::time::Instant::now() >= deadline {
                break;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    }

    // Let the tail of the fixture finalize before closing.
    wait_until(Duration::from_secs(20), || {
        progress.load(Ordering::SeqCst) + 50 >= total
    })
    .await;

    stream.close().await.expect("close succeeds");
    let _ = tokio::time::timeout(Duration::from_secs(10), collector).await;

    let joined = transcript.lock().expect("transcript poisoned").join(" ");

    RunResult {
        transcript: joined,
        connections: mock.connection_count(),
        reconnects: reconnects.load(Ordering::SeqCst) as usize,
    }
}

/// Kill points spread across the run, reproducible from a seed.
fn chaos_modes(seed: u64, kills: usize) -> Vec<MockMode> {
    let mut generator = Lcg::new(seed);
    (0..kills)
        .map(|_| MockMode::DisconnectAfter(20 + generator.next_below(60) as usize))
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reconnects_keep_word_error_under_one_percent() {
    let script = TranscriptScript::fixture();

    let baseline = run(&script, Vec::new()).await;
    assert_eq!(
        baseline.connections, 1,
        "the baseline run must not have reconnected"
    );
    assert_eq!(
        word_error_rate(&script.expected_text(), &baseline.transcript),
        0.0,
        "the uninterrupted run must reproduce the fixture exactly, or the \
         comparison below measures the mock rather than the reconnect logic"
    );

    let chaotic = run(&script, chaos_modes(0xC0FFEE, 5)).await;
    assert!(
        chaotic.reconnects >= 5,
        "expected at least five reconnects, saw {}",
        chaotic.reconnects
    );
    assert_eq!(
        chaotic.connections,
        chaotic.reconnects + 1,
        "every reconnect should have produced exactly one new connection"
    );

    let wer = word_error_rate(&baseline.transcript, &chaotic.transcript);
    assert!(
        wer < 0.01,
        "STT-09 requires WER under 1% against an uninterrupted run; got {:.4}\n\
         baseline: {}\n\
         chaotic:  {}",
        wer,
        baseline.transcript,
        chaotic.transcript
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_result_is_stable_across_different_kill_points() {
    // One seed proves the mechanism; several prove it does not depend on where
    // the sockets happened to die. The mid-utterance and between-utterance
    // cases take different paths through the deduplicator.
    let script = TranscriptScript::fixture();
    let baseline = run(&script, Vec::new()).await;

    for seed in [1u64, 7, 4_242] {
        let chaotic = run(&script, chaos_modes(seed, 4)).await;
        let wer = word_error_rate(&baseline.transcript, &chaotic.transcript);
        assert!(
            wer < 0.01,
            "seed {seed} produced WER {wer:.4}\nchaotic: {}",
            chaotic.transcript
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_dangling_partial_is_finalized_rather_than_left_open() {
    // Spec 7.3's conformance property has to survive a reconnect: the partial
    // that was in flight when the socket died must still be superseded by a
    // final, or the transcript ends with a line that never settles.
    let script = TranscriptScript::fixture();
    let chaotic = run(&script, chaos_modes(0xBEEF, 3)).await;

    let baseline = run(&script, Vec::new()).await;
    let wer = word_error_rate(&baseline.transcript, &chaotic.transcript);
    assert!(wer < 0.01, "WER {wer:.4}");
    assert!(
        chaotic
            .transcript
            .contains("reconvene next Tuesday morning"),
        "the last utterance must survive the chaos: {}",
        chaotic.transcript
    );
}
