//! The Deepgram streaming response normalizer (spec 7.4, STT-01, STT-03).
//!
//! Pure data in, canonical segments out — no socket, no runtime. The fixture is
//! handcrafted from the field names spec 7.4 pins down: `channel.alternatives[0]
//! .words[]` with `word`, `punctuated_word`, `start`, `end`, `confidence` and
//! `speaker`, plus top-level `is_final` and `speech_final`.

use fotw_stt::deepgram::{DeepgramConfig, DeepgramNormalizer, DeepgramResult};
use fotw_stt::{SegmentStore, Source, TimestampSource, TranscriptSegment};

const FIXTURE: &str = include_str!("fixtures/deepgram_streaming.json");
const EXPECTED: &str = include_str!("fixtures/deepgram_streaming_expected.json");

fn fixture_messages() -> Vec<DeepgramResult> {
    serde_json::from_str(FIXTURE).expect("the Deepgram fixture parses")
}

fn fixture_config() -> DeepgramConfig {
    DeepgramConfig {
        session_id: "fixture-session".to_string(),
        source: Source::System,
        model: "nova-3".to_string(),
        language: Some("en".to_string()),
        diarization_enabled: true,
    }
}

fn normalize_fixture() -> Vec<TranscriptSegment> {
    // Deterministic ids so the golden file is writable by hand; production mints
    // ULIDs.
    let mut normalizer = DeepgramNormalizer::with_test_ids(fixture_config(), "seg");
    normalizer.normalize_all(&fixture_messages())
}

#[test]
fn golden_file_the_fixture_normalizes_to_exactly_the_expected_segments() {
    let produced = serde_json::to_value(normalize_fixture()).expect("segments serialize");
    let expected: serde_json::Value =
        serde_json::from_str(EXPECTED).expect("the golden file parses");

    // Compare element by element so a failure names the segment that drifted
    // rather than dumping seven of them.
    let produced = produced.as_array().expect("an array of segments");
    let expected = expected.as_array().expect("an array of segments");
    for (index, (got, want)) in produced.iter().zip(expected.iter()).enumerate() {
        assert_eq!(
            got,
            want,
            "segment {index} drifted\n  got:  {}\n  want: {}",
            serde_json::to_string(got).unwrap(),
            serde_json::to_string(want).unwrap()
        );
    }
    assert_eq!(produced.len(), expected.len(), "wrong number of segments");
}

#[test]
fn deepgram_seconds_become_milliseconds() {
    // Deepgram reports start/end in SECONDS; every consumer above this crate
    // works in integer milliseconds from session t0.
    let segments = normalize_fixture();
    let first = &segments[0];

    // Fixture word 0 is start 1.02 / end 1.18 seconds.
    assert_eq!(first.words[0].start_ms, 1_020);
    assert_eq!(first.words[0].end_ms, 1_180);
    assert_eq!(first.start_ms, 1_020);
    assert_eq!(first.end_ms, 1_620);
    assert_eq!(first.timestamp_source, TimestampSource::Provider);
}

#[test]
fn punctuated_word_is_preferred_over_the_raw_word() {
    // Spec 7.4: "Prefer `punctuated_word`". The raw `word` is lowercased and
    // unpunctuated, which reads badly and breaks quote matching in the summary.
    let segments = normalize_fixture();
    let last = segments.last().expect("segments were produced");

    assert_eq!(last.words[0].text, "Great.");
    assert_eq!(last.words[2].text, "else?");
}

#[test]
fn a_word_without_punctuated_word_falls_back_to_the_raw_word() {
    // `smart_format=false` or an older model omits it entirely.
    let raw = r#"[{
        "type": "Results", "is_final": true, "speech_final": true,
        "channel": { "alternatives": [{
            "transcript": "hello", "confidence": 0.9,
            "words": [{ "word": "hello", "start": 0.5, "end": 0.9, "confidence": 0.9 }]
        }]}
    }]"#;
    let messages: Vec<DeepgramResult> = serde_json::from_str(raw).expect("parses");
    let segments =
        DeepgramNormalizer::with_test_ids(fixture_config(), "seg").normalize_all(&messages);

    assert_eq!(segments[0].words[0].text, "hello");
    assert_eq!(segments[0].words[0].speaker, None);
    assert_eq!(segments[0].speaker, None);
}

#[test]
fn empty_interim_results_produce_nothing() {
    // Deepgram emits empty interims during silence. Turning those into segments
    // would put blank lines in the transcript and bump the revision for no reason.
    let messages = fixture_messages();
    assert_eq!(messages.len(), 8);
    assert_eq!(
        normalize_fixture().len(),
        7,
        "the empty interim must not become a segment"
    );
}

#[test]
fn partials_share_the_finals_id_and_the_store_collapses_them() {
    // Rule 4 end to end through a real provider shape.
    let mut store = SegmentStore::new();
    for segment in normalize_fixture() {
        store.upsert(segment);
    }

    assert_eq!(store.len(), 3, "five emissions of one utterance, one line");
    let first = store.get("seg-0").expect("seg-0 present");
    assert_eq!(first.text, "So I think we should ship on Friday.");
    assert_eq!(first.revision, 4);
    assert!(first.is_final);
    assert!(
        store.pending_ids().is_empty(),
        "every partial was superseded by a final"
    );
}

#[test]
fn a_finalized_chunk_that_is_not_speech_final_keeps_the_utterance_open() {
    // Deepgram's `is_final` finalizes a *chunk*; `speech_final` ends the
    // *utterance*. Treating the first as the second would chop one sentence into
    // several transcript lines at arbitrary boundaries.
    let segments = normalize_fixture();

    // The third emission is the is_final-but-not-speech_final chunk.
    assert!(!segments[2].is_final);
    assert_eq!(segments[2].text, "So I think we should");
    // Its text is then carried forward into the next partial rather than lost.
    assert!(segments[3].text.starts_with("So I think we should ship"));
    assert_eq!(segments[4].id, segments[2].id);
    assert!(segments[4].is_final);
}

#[test]
fn a_mic_stream_without_diarization_forces_every_speaker_to_me() {
    let config = DeepgramConfig {
        source: Source::Mic,
        diarization_enabled: false,
        ..fixture_config()
    };
    let segments =
        DeepgramNormalizer::with_test_ids(config, "seg").normalize_all(&fixture_messages());

    for segment in &segments {
        assert_eq!(segment.speaker.as_deref(), Some("me"));
        for word in &segment.words {
            assert_eq!(word.speaker.as_deref(), Some("me"));
        }
    }
}

#[test]
fn a_segment_whose_words_disagree_on_the_speaker_reports_none() {
    // Deepgram can put two speakers inside one alternative. Picking either one
    // would attribute somebody's words to the wrong person, which is worse than
    // admitting we do not know.
    let segments = normalize_fixture();
    let mixed = segments.last().expect("segments were produced");

    assert_eq!(mixed.speaker, None);
    assert_eq!(mixed.words[0].speaker.as_deref(), Some("S1"));
    assert_eq!(mixed.words[1].speaker.as_deref(), Some("S0"));
}

#[test]
fn timestamps_stay_continuous_across_a_reconnect() {
    // Rule 1 through the real adapter path: the socket drops after the fixture,
    // STT-09 replays PCM into a new connection whose clock restarts at zero, and
    // the same fixture arrives again.
    let mut normalizer = DeepgramNormalizer::with_test_ids(fixture_config(), "seg");
    let messages = fixture_messages();

    let before = normalizer.normalize_all(&messages);
    let last_before = before.last().expect("segments before the drop").end_ms;
    assert_eq!(last_before, 5_100);

    // The new connection starts from the last final we had, per STT-09.
    let dangling = normalizer.reconnected_at(last_before);
    assert!(
        dangling.is_none(),
        "the fixture ends on a final, so nothing was left open"
    );

    let after = normalizer.normalize_all(&messages);

    for segment in &after {
        assert!(
            segment.start_ms >= last_before,
            "a post-reconnect segment landed back at {} ms, before the reconnect at {last_before} ms",
            segment.start_ms
        );
    }
    assert_eq!(after[0].start_ms, last_before + 1_020);
    assert_eq!(after.last().unwrap().end_ms, last_before + 5_100);

    // And the ids do not collide with the pre-reconnect ones, so replayed audio
    // cannot overwrite an already-finalized line.
    assert_eq!(after[0].id, "seg-3");
}

#[test]
fn a_reconnect_mid_utterance_finalizes_the_dangling_partial() {
    // Otherwise the last partial before the drop sits in the store forever
    // marked non-final, and the "every partial is superseded" conformance check
    // fails at end of session.
    let mut normalizer = DeepgramNormalizer::with_test_ids(fixture_config(), "seg");
    let messages = fixture_messages();

    // Feed only the first three messages: the utterance is still open.
    normalizer.normalize_all(&messages[..3]);

    let closed = normalizer
        .reconnected_at(2_160)
        .expect("the open utterance was finalized");
    assert_eq!(closed.id, "seg-0");
    assert!(closed.is_final);
    assert_eq!(closed.revision, 3);
    assert_eq!(closed.text, "So I think we should");
}

#[test]
fn finishing_the_stream_finalizes_anything_still_open() {
    let mut normalizer = DeepgramNormalizer::with_test_ids(fixture_config(), "seg");
    normalizer.normalize_all(&fixture_messages()[..2]);

    let closed = normalizer
        .finish()
        .expect("the open utterance was finalized");
    assert!(closed.is_final);
    assert_eq!(closed.revision, 2);

    assert!(normalizer.finish().is_none(), "finish() is idempotent");
}

#[test]
fn a_transcript_with_no_word_timings_falls_back_to_the_chunk_times() {
    let raw = r#"[{
        "type": "Results", "is_final": true, "speech_final": true,
        "start": 12.5, "duration": 1.25,
        "channel": { "alternatives": [{ "transcript": "no words here", "confidence": 0.6 }]}
    }]"#;
    let messages: Vec<DeepgramResult> = serde_json::from_str(raw).expect("parses");
    let segments =
        DeepgramNormalizer::with_test_ids(fixture_config(), "seg").normalize_all(&messages);

    assert_eq!(segments[0].start_ms, 12_500);
    assert_eq!(segments[0].end_ms, 13_750);
    assert!(segments[0].words.is_empty(), "words: [] is legal");
    assert_eq!(
        segments[0].timestamp_source,
        TimestampSource::Provider,
        "the chunk times are still the provider's"
    );
}

#[test]
fn a_transcript_with_no_timings_at_all_is_marked_estimated() {
    // Rule 3: times synthesized from our own clock are never labelled as the
    // provider's.
    let raw = r#"[{
        "type": "Results", "is_final": true, "speech_final": true,
        "channel": { "alternatives": [{ "transcript": "untimed", "confidence": 0.6 }]}
    }]"#;
    let messages: Vec<DeepgramResult> = serde_json::from_str(raw).expect("parses");
    let mut normalizer = DeepgramNormalizer::with_test_ids(fixture_config(), "seg");
    let segments = normalizer.normalize_at(&messages, Some(9_000));

    assert_eq!(segments[0].timestamp_source, TimestampSource::Estimated);
    assert_eq!(segments[0].start_ms, 0);
    assert_eq!(segments[0].end_ms, 9_000);
}

#[test]
fn non_results_messages_are_ignored() {
    // The same socket carries Metadata, SpeechStarted and UtteranceEnd frames.
    // None of them is a transcript, and none of them may crash the normalizer.
    let raw = r#"[
        { "type": "Metadata", "request_id": "abc" },
        { "type": "SpeechStarted", "timestamp": 1.0 },
        { "type": "UtteranceEnd", "last_word_end": 3.1 }
    ]"#;
    let messages: Vec<DeepgramResult> = serde_json::from_str(raw).expect("parses");
    let segments =
        DeepgramNormalizer::with_test_ids(fixture_config(), "seg").normalize_all(&messages);

    assert!(segments.is_empty());
}

#[test]
fn malformed_provider_json_is_a_retryable_server_error_not_a_panic() {
    use fotw_stt::{FailoverPolicy, SttErrorClass};

    let mut normalizer = DeepgramNormalizer::with_test_ids(fixture_config(), "seg");
    let error = normalizer
        .push_json("{\"type\":\"Results\",\"channel\":")
        .expect_err("truncated JSON is an error");

    assert_eq!(error.class, SttErrorClass::Server);
    assert_eq!(error.provider, "deepgram");
    assert!(error.retryable);
    assert_eq!(error.failover_policy(), FailoverPolicy::Backoff);
    assert!(
        error.detail.is_some(),
        "the parse error is kept for the log"
    );
}

#[test]
fn push_json_normalizes_a_single_frame() {
    let mut normalizer = DeepgramNormalizer::with_test_ids(fixture_config(), "seg");
    let frame = serde_json::to_string(&fixture_messages()[0]).expect("re-serializes");

    let segment = normalizer
        .push_json(&frame)
        .expect("valid JSON")
        .expect("a segment");

    assert_eq!(segment.text, "so i think");
    assert_eq!(segment.provider, "deepgram");
}

#[test]
fn the_default_config_diarizes_the_system_stream_but_not_the_mic() {
    // The two-stream default (spec 7.5): the mic stream skips diarization
    // entirely, which is half of why the split is worth paying for.
    assert!(DeepgramConfig::new("s", Source::System).diarization_enabled);
    assert!(!DeepgramConfig::new("s", Source::Mic).diarization_enabled);
    assert_eq!(DeepgramConfig::new("s", Source::Mic).model, "nova-3");
}
