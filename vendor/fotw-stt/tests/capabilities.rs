//! The capability descriptor (spec 7.3, STT-02).
//!
//! > The UI reads `capabilities` to grey out diarization- and
//! > timestamp-dependent features rather than failing at runtime.
//!
//! So this is a UI contract as much as an adapter one, and its wire shape has to
//! match the TypeScript type in the spec.

use fotw_stt::{CustomVocabulary, FeatureAvailability, RetentionControl, SttCapabilities};

#[test]
fn capabilities_serialize_with_the_spec_field_names() {
    let capabilities = SttCapabilities {
        streaming: true,
        batch: true,
        word_timestamps: FeatureAvailability::Both,
        diarization: FeatureAvailability::Both,
        language_detection: true,
        custom_vocabulary: CustomVocabulary::Keyterm,
        max_file_bytes: 2_000_000_000,
        max_file_seconds: 62_500,
        native_rates: vec![8_000, 16_000, 24_000, 48_000],
        retention_control: RetentionControl::Param,
        supports_replay_faster_than_realtime: true,
    };

    let json = serde_json::to_value(&capabilities).expect("serializes");
    assert_eq!(
        json,
        serde_json::json!({
            "streaming": true,
            "batch": true,
            "wordTimestamps": "both",
            "diarization": "both",
            "languageDetection": true,
            "customVocabulary": "keyterm",
            "maxFileBytes": 2_000_000_000_u64,
            "maxFileSeconds": 62_500,
            "nativeRates": [8_000, 16_000, 24_000, 48_000],
            "retentionControl": "param",
            "supportsReplayFasterThanRealtime": true,
        })
    );

    let back: SttCapabilities = serde_json::from_value(json).expect("round-trips");
    assert_eq!(back, capabilities);
}

#[test]
fn the_availability_enums_use_the_spec_wire_tags() {
    for (value, tag) in [
        (FeatureAvailability::Both, "both"),
        (FeatureAvailability::Streaming, "streaming"),
        (FeatureAvailability::Batch, "batch"),
        (FeatureAvailability::None, "none"),
    ] {
        assert_eq!(serde_json::to_value(value).unwrap(), tag);
    }

    for (value, tag) in [
        (CustomVocabulary::Keyterm, "keyterm"),
        (CustomVocabulary::Keywords, "keywords"),
        (CustomVocabulary::Prompt, "prompt"),
        (CustomVocabulary::None, "none"),
    ] {
        assert_eq!(serde_json::to_value(value).unwrap(), tag);
    }

    for (value, tag) in [
        (RetentionControl::Param, "param"),
        (RetentionControl::Header, "header"),
        (RetentionControl::Contract, "contract"),
        (RetentionControl::None, "none"),
    ] {
        assert_eq!(serde_json::to_value(value).unwrap(), tag);
    }
}

#[test]
fn availability_answers_the_question_the_ui_actually_asks() {
    // The UI asks "can I show word timestamps *in this mode*", not "does this
    // provider support them somewhere".
    assert!(FeatureAvailability::Both.in_streaming());
    assert!(FeatureAvailability::Both.in_batch());

    assert!(FeatureAvailability::Streaming.in_streaming());
    assert!(!FeatureAvailability::Streaming.in_batch());

    assert!(!FeatureAvailability::Batch.in_streaming());
    assert!(FeatureAvailability::Batch.in_batch());

    assert!(!FeatureAvailability::None.in_streaming());
    assert!(!FeatureAvailability::None.in_batch());
}

#[test]
fn deepgram_capabilities_match_the_provider_comparison_table() {
    let deepgram = SttCapabilities::deepgram();

    assert!(deepgram.streaming);
    assert!(deepgram.batch);
    // Word timestamps on both, per spec 7.1.
    assert!(deepgram.word_timestamps.in_streaming());
    assert!(deepgram.word_timestamps.in_batch());
    // Diarization on both, but v1 on streaming and v2 batch-only (spec 7.4).
    assert!(deepgram.diarization.in_streaming());
    assert!(deepgram.diarization.in_batch());
    // `mip_opt_out=true` is a query parameter, which is why retention control is
    // usable at all — ElevenLabs' equivalent is enterprise-contract only.
    assert_eq!(deepgram.retention_control, RetentionControl::Param);
    assert_eq!(deepgram.custom_vocabulary, CustomVocabulary::Keyterm);
    assert_eq!(deepgram.max_file_bytes, 2_000_000_000);
    assert!(deepgram.native_rates.contains(&16_000));
}

#[test]
fn replay_faster_than_realtime_decides_the_stall_recovery_strategy() {
    // STT-09 recovers a stalled stream by replaying buffered PCM into a fresh
    // connection. That only closes the gap if the provider will accept audio
    // faster than wall-clock; otherwise recovery takes as long as the outage and
    // the recovery strategy has to be "re-transcribe the file afterwards".
    let deepgram = SttCapabilities::deepgram();
    assert!(deepgram.supports_replay_faster_than_realtime);
    assert!(deepgram.can_replay_to_catch_up());

    let realtime_only = SttCapabilities {
        supports_replay_faster_than_realtime: false,
        ..SttCapabilities::deepgram()
    };
    assert!(!realtime_only.can_replay_to_catch_up());

    // A batch-only provider cannot catch a stream up no matter what, because
    // there is no stream to replay into.
    let batch_only = SttCapabilities {
        streaming: false,
        supports_replay_faster_than_realtime: true,
        ..SttCapabilities::deepgram()
    };
    assert!(!batch_only.can_replay_to_catch_up());
}
