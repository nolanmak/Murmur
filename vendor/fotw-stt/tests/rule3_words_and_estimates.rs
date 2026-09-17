//! Spec 7.2, normalization rule 3.
//!
//! > `words: []` is legal and `timestampSource: 'estimated'` is set when times
//! > were synthesized from our audio-clock position at delta arrival.
//!
//! This is the rule that keeps OpenAI's realtime transcription — which returns
//! no word timings, no speaker and no confidence (spec 7.4, "degraded by
//! contract") — inside the canonical format instead of forking it. The cost of
//! admitting it is that the UI has to be able to tell a real timestamp from a
//! synthesized one, which is what `timestampSource` is for.

use fotw_stt::{ArrivalEstimator, Source, TimeSpan, TimestampSource, TranscriptSegment, Word};

#[test]
fn a_segment_with_no_words_is_legal_and_round_trips() {
    let mut segment =
        TranscriptSegment::new("session", Source::System, "openai", "gpt-live-transcribe");
    segment.text = "so that's the plan for Q3".to_string();
    segment.start_ms = 4_000;
    segment.end_ms = 6_400;
    segment.timestamp_source = TimestampSource::Estimated;

    assert!(segment.words.is_empty());

    let json = serde_json::to_value(&segment).expect("serializes");
    // An empty array, not a missing key and not null: the TS type is `Word[]`.
    assert_eq!(json["words"], serde_json::json!([]));
    assert_eq!(json["timestampSource"], "estimated");
    assert_eq!(json["confidence"], serde_json::Value::Null);
    assert_eq!(json["speaker"], serde_json::Value::Null);

    let back: TranscriptSegment = serde_json::from_value(json).expect("round-trips");
    assert_eq!(back, segment);
}

#[test]
fn provider_supplied_times_are_marked_provider() {
    let span = TimeSpan::provided(10_520, 11_040);

    assert_eq!(span.source, TimestampSource::Provider);
    assert_eq!(span.start_ms, 10_520);
    assert_eq!(span.end_ms, 11_040);
    assert_eq!(span.duration_ms(), 520);
}

#[test]
fn synthesized_times_are_marked_estimated_and_tile_the_timeline() {
    // The OpenAI adapter's whole timing story: each delta gets the stretch of
    // audio clock between the previous delta's arrival and this one's.
    let mut estimator = ArrivalEstimator::new();

    let first = estimator.next_span(2_400);
    let second = estimator.next_span(5_100);
    let third = estimator.next_span(5_900);

    for span in [first, second, third] {
        assert_eq!(
            span.source,
            TimestampSource::Estimated,
            "arrival-derived times are never provider times"
        );
    }

    assert_eq!((first.start_ms, first.end_ms), (0, 2_400));
    // No gaps and no overlap: each span begins exactly where the last ended.
    assert_eq!(second.start_ms, first.end_ms);
    assert_eq!(third.start_ms, second.end_ms);
    assert_eq!(third.end_ms, 5_900);
}

#[test]
fn an_estimator_can_be_anchored_mid_session_and_re_anchored_on_reconnect() {
    // A provider that fails over mid-meeting starts estimating from wherever the
    // last provider stopped, not from zero.
    let mut estimator = ArrivalEstimator::starting_at(600_000);
    assert_eq!(estimator.next_span(601_200).start_ms, 600_000);

    estimator.reconnected_at(900_000);
    let after = estimator.next_span(901_000);
    assert_eq!((after.start_ms, after.end_ms), (900_000, 901_000));
}

#[test]
fn an_arrival_that_moves_backwards_cannot_produce_an_inverted_span() {
    let mut estimator = ArrivalEstimator::new();
    estimator.next_span(5_000);

    let jittered = estimator.next_span(4_000);

    assert_eq!(jittered.start_ms, 5_000);
    assert_eq!(jittered.end_ms, 5_000);
    assert_eq!(jittered.duration_ms(), 0);
    assert_eq!(estimator.last_end_ms(), 5_000);
}

#[test]
fn words_present_means_the_timestamps_are_the_providers() {
    // The contrast case: Deepgram reports per-word timings, so nothing is
    // synthesized and the UI can offer click-to-seek on individual words.
    let mut segment = TranscriptSegment::new("session", Source::System, "deepgram", "nova-3");
    segment.words = vec![
        Word {
            text: "Hello".to_string(),
            start_ms: 10_520,
            end_ms: 10_860,
            confidence: Some(0.99),
            speaker: Some("S0".to_string()),
        },
        Word {
            text: "there.".to_string(),
            start_ms: 10_860,
            end_ms: 11_040,
            confidence: Some(0.97),
            speaker: Some("S0".to_string()),
        },
    ];
    segment.start_ms = 10_520;
    segment.end_ms = 11_040;

    assert_eq!(segment.timestamp_source, TimestampSource::Provider);
    assert_eq!(segment.duration_ms(), 520);
    assert_eq!(segment.words[0].duration_ms(), 340);
}
