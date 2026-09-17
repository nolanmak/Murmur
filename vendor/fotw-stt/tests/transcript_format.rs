//! The canonical internal transcript format (spec 7.2, STT-01).
//!
//! Every adapter normalizes into these types, and the wire shape must match the
//! TypeScript interface in the spec exactly (camelCase, lowercase enum tags) so
//! the generated TS bindings and the daemon's WebSocket payloads agree.

use fotw_stt::{Source, TimestampSource, TranscriptSegment, Word};

#[test]
fn word_serializes_with_the_spec_field_names() {
    let word = Word {
        text: "Hello".to_string(),
        start_ms: 10_520,
        end_ms: 10_860,
        confidence: Some(0.99),
        speaker: Some("S0".to_string()),
    };

    let json = serde_json::to_value(&word).expect("Word serializes");
    assert_eq!(
        json,
        serde_json::json!({
            "text": "Hello",
            "startMs": 10_520,
            "endMs": 10_860,
            "confidence": 0.99,
            "speaker": "S0",
        })
    );

    let round_tripped: Word = serde_json::from_value(json).expect("Word round-trips");
    assert_eq!(round_tripped, word);
}

#[test]
fn a_null_confidence_and_speaker_survive_the_round_trip() {
    // OpenAI streaming has neither. `null` must stay `null`, not vanish, or the
    // UI cannot tell "no confidence reported" from "confidence 0".
    let word = Word {
        text: "um".to_string(),
        start_ms: 0,
        end_ms: 120,
        confidence: None,
        speaker: None,
    };

    let json = serde_json::to_value(&word).expect("Word serializes");
    assert_eq!(json["confidence"], serde_json::Value::Null);
    assert_eq!(json["speaker"], serde_json::Value::Null);
}

#[test]
fn transcript_segment_serializes_with_the_spec_field_names() {
    let mut segment = TranscriptSegment::new("01J0SESSION", Source::System, "deepgram", "nova-3");
    segment.id = "01J0SEGMENT".to_string();
    segment.speaker = Some("S1".to_string());
    segment.text = "Hello there.".to_string();
    segment.start_ms = 10_520;
    segment.end_ms = 11_040;
    segment.confidence = Some(0.98);
    segment.language = Some("en".to_string());
    segment.is_final = true;
    segment.revision = 2;
    segment.timestamp_source = TimestampSource::Provider;
    segment.words = vec![Word {
        text: "Hello".to_string(),
        start_ms: 10_520,
        end_ms: 10_860,
        confidence: Some(0.99),
        speaker: Some("S1".to_string()),
    }];

    let json = serde_json::to_value(&segment).expect("TranscriptSegment serializes");
    assert_eq!(
        json,
        serde_json::json!({
            "id": "01J0SEGMENT",
            "sessionId": "01J0SESSION",
            "source": "system",
            "speaker": "S1",
            "text": "Hello there.",
            "startMs": 10_520,
            "endMs": 11_040,
            "words": [{
                "text": "Hello",
                "startMs": 10_520,
                "endMs": 10_860,
                "confidence": 0.99,
                "speaker": "S1",
            }],
            "confidence": 0.98,
            "language": "en",
            "isFinal": true,
            "revision": 2,
            "provider": "deepgram",
            "model": "nova-3",
            "timestampSource": "provider",
        })
    );

    let round_tripped: TranscriptSegment =
        serde_json::from_value(json).expect("TranscriptSegment round-trips");
    assert_eq!(round_tripped, segment);
}

#[test]
fn source_and_timestamp_source_use_the_spec_wire_tags() {
    assert_eq!(serde_json::to_value(Source::Mic).unwrap(), "mic");
    assert_eq!(serde_json::to_value(Source::System).unwrap(), "system");
    assert_eq!(
        serde_json::to_value(TimestampSource::Provider).unwrap(),
        "provider"
    );
    assert_eq!(
        serde_json::to_value(TimestampSource::Estimated).unwrap(),
        "estimated"
    );
}

#[test]
fn a_new_segment_gets_a_ulid_and_provider_timestamps_by_default() {
    let a = TranscriptSegment::new("session", Source::Mic, "deepgram", "nova-3");
    let b = TranscriptSegment::new("session", Source::Mic, "deepgram", "nova-3");

    // ULIDs are 26 characters of Crockford base32 and are unique per segment.
    assert_eq!(a.id.len(), 26, "segment id is a ULID: {}", a.id);
    assert_ne!(a.id, b.id);

    // ULIDs are lexicographically sortable by creation time, which is what makes
    // them usable as the transcript's ordering key.
    assert!(a.id <= b.id);

    assert_eq!(a.revision, 0);
    assert!(!a.is_final);
    assert!(a.words.is_empty());
    assert_eq!(a.timestamp_source, TimestampSource::Provider);
}
