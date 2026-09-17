//! The parts of the Deepgram transport that are pure (spec 7.4, STT-09,
//! STT-12).
//!
//! No socket, no runtime, no clock. Everything the reconnect path can get
//! subtly wrong — the backoff schedule, the attempt budget, the ring's clamping,
//! the deduplication arithmetic, the failure classification — is arithmetic, and
//! is asserted here exactly rather than inferred from a stream's behaviour.

use fotw_stt::backoff::{BackoffPolicy, FixedJitter, Jitter, ProcessJitter, ReconnectBudget};
use fotw_stt::dedupe::{TranscriptTail, normalize_tokens, trim_leading_tokens};
use fotw_stt::deepgram_wire::{
    BATCH_ONLY_DIARIZE_MODEL, DeepgramEndpoint, DeepgramErrorFrame, DeepgramStreamParams,
    KEEPALIVE_FRAME, STREAMING_DIARIZE_MODEL, extract_deepgram_code, map_close, map_http_status,
};
use fotw_stt::replay::{PcmRing, to_linear16_le};
use fotw_stt::{FailoverPolicy, Source, SttErrorClass, TranscriptSegment, Word};

// ---------------------------------------------------------------------------
// §7.4 query parameters
// ---------------------------------------------------------------------------

fn query_pairs(query: &str) -> Vec<(String, String)> {
    query
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect()
}

#[test]
fn query_carries_every_parameter_the_spec_lists() {
    let query = DeepgramStreamParams::spec().to_query();
    for expected in [
        "model=nova-3",
        "encoding=linear16",
        "sample_rate=16000",
        "channels=1",
        "interim_results=true",
        "punctuate=true",
        "smart_format=true",
        "endpointing=300",
        "utterance_end_ms=1000",
        "vad_events=true",
        "mip_opt_out=true",
    ] {
        assert!(
            query.contains(expected),
            "spec 7.4 requires {expected} on every streaming request, got {query}"
        );
    }
}

#[test]
fn mip_opt_out_is_present_on_every_variation_of_the_request() {
    // The retention opt-out is what keeps meeting audio out of Deepgram's
    // model-improvement program (spec 10). It must not be contingent on any
    // other setting being on.
    let variations = [
        DeepgramStreamParams::spec(),
        DeepgramStreamParams::spec().with_diarize(false),
        DeepgramStreamParams::spec().with_keyterms(["Acme"]),
        DeepgramStreamParams::spec().with_language("en-GB"),
        DeepgramStreamParams::spec()
            .with_diarize(false)
            .with_keyterms(Vec::<String>::new()),
    ];
    for params in variations {
        let pairs = query_pairs(&params.to_query());
        assert!(
            pairs
                .iter()
                .any(|(key, value)| key == "mip_opt_out" && value == "true"),
            "mip_opt_out=true is unconditional"
        );
    }
}

/// Deepgram refuses the handshake when `diarize_model` accompanies `diarize`:
/// "diarize_model cannot be used together with diarize or diarize_version."
/// It is an HTTP 400 on connect, so nothing is transcribed at all — and since
/// `session::run` consumes only `StreamEvent::Final`, an empty transcript was
/// indistinguishable from a meeting where nobody spoke. Every recording this
/// project ever made came back with zero segments because of this pair.
#[test]
fn streaming_sends_diarize_alone_and_never_the_model_alongside_it() {
    let query = DeepgramStreamParams::spec().to_query();
    assert!(query.contains("diarize=true"));
    assert!(
        !query.contains("diarize_model"),
        "diarize_model alongside diarize is a 400 on connect: {query}"
    );
    assert!(!query.contains(BATCH_ONLY_DIARIZE_MODEL));
}

/// The setter still exists and still guards the batch-only model, because a
/// caller who reaches for it deserves the error rather than silence.
#[test]
fn the_streaming_diarize_model_is_still_the_supported_one() {
    assert_eq!(
        DeepgramStreamParams::spec().diarize_model(),
        STREAMING_DIARIZE_MODEL
    );
}

#[test]
fn diarize_model_v2_is_rejected_as_unsupported_rather_than_sent() {
    let error = DeepgramStreamParams::spec()
        .with_diarize_model(BATCH_ONLY_DIARIZE_MODEL)
        .expect_err("v2 is batch-only");

    assert_eq!(error.class, SttErrorClass::Unsupported);
    // `Surface`, not `Failover`: every provider would reject our own bad
    // request identically, so failing over would hide the defect.
    assert_eq!(error.failover_policy(), FailoverPolicy::Surface);
    assert!(!error.retryable);
}

#[test]
fn diarize_parameters_disappear_together_when_diarization_is_off() {
    // The mic stream under the two-stream default is one known person, so
    // diarizing it is paid error. Sending `diarize_model` with `diarize` off
    // would look enabled in a URL diff while doing nothing.
    let query = DeepgramStreamParams::spec().with_diarize(false).to_query();
    assert!(!query.contains("diarize=true"));
    assert!(!query.contains("diarize_model"));
}

#[test]
fn keyterms_repeat_and_are_percent_encoded() {
    let query = DeepgramStreamParams::spec()
        .with_keyterms(["Fly on the Wall", "Deepgram"])
        .to_query();
    let keyterms: Vec<String> = query_pairs(&query)
        .into_iter()
        .filter(|(key, _)| key == "keyterm")
        .map(|(_, value)| value)
        .collect();

    assert_eq!(keyterms, vec!["Fly%20on%20the%20Wall", "Deepgram"]);
}

#[test]
fn endpoint_is_injectable_and_production_stays_encrypted() {
    let production = DeepgramEndpoint::production();
    assert_eq!(
        production.url_with("model=nova-3"),
        "wss://api.deepgram.com/v1/listen?model=nova-3"
    );
    assert!(production.is_secure());

    let mock = DeepgramEndpoint::loopback(41_234);
    assert_eq!(
        mock.url_with("model=nova-3"),
        "ws://127.0.0.1:41234/v1/listen?model=nova-3"
    );
    assert!(!mock.is_secure());
}

#[test]
fn keepalive_frame_is_the_exact_json_the_provider_expects() {
    assert_eq!(KEEPALIVE_FRAME, r#"{"type":"KeepAlive"}"#);
}

// ---------------------------------------------------------------------------
// Backoff (STT-09)
// ---------------------------------------------------------------------------

#[test]
fn backoff_doubles_from_250ms_and_caps_at_8s() {
    let policy = BackoffPolicy::spec();
    let schedule: Vec<u64> = (0..8).map(|n| policy.unjittered_delay_ms(n)).collect();
    assert_eq!(
        schedule,
        vec![250, 500, 1_000, 2_000, 4_000, 8_000, 8_000, 8_000]
    );
}

#[test]
fn jitter_is_symmetric_and_bounded_at_twenty_percent() {
    let policy = BackoffPolicy::spec();
    assert_eq!(policy.delay_ms(0, 0.0), 200, "the low end of ±20% of 250ms");
    assert_eq!(policy.delay_ms(0, 0.5), 250, "the midpoint is unjittered");
    assert_eq!(
        policy.delay_ms(0, 1.0),
        300,
        "the high end of ±20% of 250ms"
    );

    for attempt in 0..8 {
        let base = policy.unjittered_delay_ms(attempt) as f64;
        for step in 0..=20 {
            let delay = policy.delay_ms(attempt, f64::from(step) / 20.0) as f64;
            assert!(
                delay >= base * 0.8 - 1.0 && delay <= base * 1.2 + 1.0,
                "attempt {attempt} produced {delay}ms, outside ±20% of {base}ms"
            );
        }
    }
}

#[test]
fn process_jitter_stays_in_the_unit_interval_and_is_seedable() {
    let mut generator = ProcessJitter::from_seed(0x5EED);
    let draws: Vec<f64> = (0..500).map(|_| generator.unit()).collect();
    assert!(draws.iter().all(|draw| (0.0..1.0).contains(draw)));

    let mut replay = ProcessJitter::from_seed(0x5EED);
    let again: Vec<f64> = (0..500).map(|_| replay.unit()).collect();
    assert_eq!(draws, again, "an explicit seed must be reproducible");

    // A zero seed is a xorshift fixed point; it must not survive construction.
    let mut zero = ProcessJitter::from_seed(0);
    assert!(zero.unit() > 0.0);
}

#[test]
fn fixed_jitter_pins_the_draw() {
    assert_eq!(FixedJitter(0.5).unit(), 0.5);
}

#[test]
fn budget_allows_six_attempts_per_rolling_sixty_seconds() {
    let mut budget = BackoffPolicy::spec().budget();

    // Six attempts inside the window, numbered so they drive the exponent.
    for expected in 0..6u32 {
        assert_eq!(
            budget.try_record(u64::from(expected) * 1_000),
            Some(expected)
        );
    }
    assert_eq!(
        budget.try_record(6_000),
        None,
        "a seventh attempt inside 60s must be refused"
    );
    assert_eq!(budget.remaining(6_000), 0);
}

#[test]
fn budget_refills_as_the_window_slides() {
    let mut budget = ReconnectBudget::new(6, 60_000);
    for attempt in 0..6 {
        budget.try_record(attempt * 1_000);
    }
    assert_eq!(budget.try_record(59_000), None);

    // The first attempt (t=0) leaves the window at t=60_000.
    assert_eq!(budget.try_record(60_001), Some(5));
    assert_eq!(budget.remaining(60_001), 0);

    // Long after everything expired, the budget is whole again.
    assert_eq!(budget.remaining(200_000), 6);
}

#[test]
fn budget_does_not_reset_on_success() {
    // A provider that accepts a socket and drops it immediately would otherwise
    // loop forever, because each attempt "succeeded" before failing.
    let mut budget = ReconnectBudget::new(6, 60_000);
    for attempt in 0..6 {
        assert!(budget.try_record(attempt * 100).is_some());
    }
    assert_eq!(budget.try_record(600), None);
}

// ---------------------------------------------------------------------------
// The replay ring (STT-09)
// ---------------------------------------------------------------------------

fn ramp(start: i16, count: usize) -> Vec<i16> {
    (0..count).map(|n| start.wrapping_add(n as i16)).collect()
}

#[test]
fn ring_tracks_the_write_head_in_milliseconds() {
    let mut ring = PcmRing::spec();
    assert_eq!(ring.written_ms(), 0);
    assert_eq!(ring.push(&ramp(0, 16_000)), 1_000);
    assert_eq!(ring.push(&ramp(0, 8_000)), 1_500);
    assert_eq!(ring.buffered_ms(), 1_500);
}

#[test]
fn ring_keeps_exactly_thirty_seconds() {
    let mut ring = PcmRing::spec();
    // 40 s in 1 s chunks.
    for _ in 0..40 {
        ring.push(&vec![0i16; 16_000]);
    }
    assert_eq!(ring.written_ms(), 40_000);
    assert_eq!(ring.earliest_ms(), 10_000);
    assert_eq!(ring.buffered_ms(), 30_000);
}

#[test]
fn replay_returns_audio_from_the_requested_position() {
    let mut ring = PcmRing::new(16_000, 30_000);
    ring.push(&ramp(0, 16_000)); // 0..1000 ms
    ring.push(&ramp(100, 16_000)); // 1000..2000 ms

    let replay = ring.replay_from(1_500);
    assert_eq!(replay.start_ms, 1_500);
    assert_eq!(replay.truncated_ms, 0);
    assert!(!replay.lost_audio());
    assert_eq!(replay.samples.len(), 8_000, "500 ms at 16 kHz");
    assert_eq!(replay.samples[0], 100i16.wrapping_add(8_000));
}

#[test]
fn replay_spans_chunk_boundaries() {
    let mut ring = PcmRing::new(16_000, 30_000);
    for chunk in 0..5 {
        ring.push(&ramp(chunk * 10, 1_600)); // 100 ms each
    }
    let replay = ring.replay_from(150);
    assert_eq!(replay.start_ms, 150);
    assert_eq!(replay.samples.len(), 5_600, "350 ms remaining at 16 kHz");
}

#[test]
fn replay_clamps_to_the_window_and_reports_what_was_lost() {
    // The clamp is why `Replay::start_ms` exists: rebasing the session clock to
    // the position we *asked* for, rather than the one we got, shifts every
    // later timestamp by the length of the outage.
    let mut ring = PcmRing::new(16_000, 1_000);
    for _ in 0..4 {
        ring.push(&vec![7i16; 16_000]);
    }
    let replay = ring.replay_from(500);
    assert_eq!(replay.start_ms, 3_000, "clamped to the retained window");
    assert_eq!(replay.truncated_ms, 2_500);
    assert!(replay.lost_audio());
    assert_eq!(replay.samples.len(), 16_000);
}

#[test]
fn replay_past_the_write_head_is_empty() {
    let mut ring = PcmRing::spec();
    ring.push(&vec![1i16; 16_000]);
    let replay = ring.replay_from(5_000);
    assert!(replay.is_empty());
    assert_eq!(replay.start_ms, 1_000);
}

#[test]
fn linear16_encoding_is_little_endian() {
    assert_eq!(
        to_linear16_le(&[1, -2, 258]),
        vec![0x01, 0x00, 0xFE, 0xFF, 0x02, 0x01]
    );
}

// ---------------------------------------------------------------------------
// Replay deduplication (STT-09)
// ---------------------------------------------------------------------------

fn word(text: &str, start_ms: u64) -> Word {
    Word {
        text: text.to_string(),
        start_ms,
        end_ms: start_ms + 300,
        confidence: Some(0.9),
        speaker: Some("S0".to_string()),
    }
}

fn segment_of(text: &str, start_ms: u64) -> TranscriptSegment {
    let mut segment = TranscriptSegment::new("session", Source::System, "deepgram", "nova-3");
    let mut cursor = start_ms;
    for token in text.split_whitespace() {
        segment.words.push(word(token, cursor));
        cursor += 300;
    }
    segment.text = text.to_string();
    segment.start_ms = start_ms;
    segment.end_ms = cursor;
    segment
}

#[test]
fn normalization_ignores_casing_and_punctuation() {
    assert_eq!(
        normalize_tokens("Okay, so — the Q3 numbers?"),
        vec!["okay", "so", "the", "q3", "numbers"]
    );
    assert_eq!(normalize_tokens("   "), Vec::<String>::new());
}

#[test]
fn tail_finds_the_longest_overlap_not_the_first() {
    // A single common word matches almost anything; trimming on it would leave
    // the rest of a duplicated utterance in the transcript twice.
    let mut tail = TranscriptTail::default();
    tail.push_text("we should revisit the pricing page");
    let overlap = tail.overlap_with_text("the pricing page before launch");
    assert_eq!(overlap, 3);
}

#[test]
fn tail_reports_no_overlap_when_the_text_is_new() {
    let mut tail = TranscriptTail::default();
    tail.push_text("quarterly revenue targets");
    assert_eq!(tail.overlap_with_text("marketing spent heavily"), 0);
}

#[test]
fn tail_is_bounded_and_drops_the_oldest_tokens() {
    let mut tail = TranscriptTail::new(4);
    tail.push_text("one two three four five six");
    assert_eq!(tail.len(), 4);
    assert_eq!(tail.overlap_with_text("five six seven"), 2);
    assert_eq!(
        tail.overlap_with_text("one two"),
        0,
        "the oldest tokens are gone"
    );
}

#[test]
fn trimming_removes_leading_words_and_moves_the_start() {
    let mut segment = segment_of("engineering shipped the audio capture rewrite", 4_000);
    assert!(trim_leading_tokens(&mut segment, 3));

    assert_eq!(segment.text, "audio capture rewrite");
    assert_eq!(segment.words.len(), 3);
    assert_eq!(segment.words[0].text, "audio");
    assert_eq!(
        segment.start_ms, 4_900,
        "the trimmed segment starts at its first surviving word"
    );
    assert_eq!(segment.end_ms, 5_800, "the end is untouched");
}

#[test]
fn a_wholly_duplicated_segment_is_reported_as_empty() {
    let mut segment = segment_of("legal flagged three vendor agreements", 0);
    assert!(
        !trim_leading_tokens(&mut segment, 5),
        "nothing survives, so the caller must drop it rather than emit a blank line"
    );
    assert!(segment.text.is_empty());
    assert!(segment.words.is_empty());
}

#[test]
fn trimming_works_on_segments_with_no_word_timings() {
    // OpenAI streaming returns `words: []`; the transcript-tail rule still has
    // to work there, so the trim cannot assume word structure.
    let mut segment = TranscriptSegment::new("session", Source::System, "openai", "gpt");
    segment.text = "the pricing page before launch".to_string();
    assert!(trim_leading_tokens(&mut segment, 3));
    assert_eq!(segment.text, "before launch");
}

#[test]
fn trimming_zero_tokens_is_a_no_op() {
    let mut segment = segment_of("data showed weekend usage climbing", 0);
    assert!(trim_leading_tokens(&mut segment, 0));
    assert_eq!(segment.text, "data showed weekend usage climbing");
}

// ---------------------------------------------------------------------------
// Failure mapping (STT-12)
// ---------------------------------------------------------------------------

#[test]
fn unauthorized_maps_to_auth_and_triggers_failover() {
    let error = map_http_status(401, None, r#"{"err_msg":"Invalid credentials."}"#);
    assert_eq!(error.class, SttErrorClass::Auth);
    assert_eq!(error.message, "Invalid credentials.");
    assert!(!error.retryable);
    assert_eq!(error.failover_policy(), FailoverPolicy::Failover);
}

#[test]
fn forbidden_also_maps_to_auth() {
    assert_eq!(
        map_http_status(403, None, "").class,
        SttErrorClass::Auth,
        "a key without the right scope is still an auth problem"
    );
}

#[test]
fn payment_required_maps_to_quota() {
    let error = map_http_status(402, None, r#"{"err_msg":"Insufficient credits."}"#);
    assert_eq!(error.class, SttErrorClass::Quota);
    assert_eq!(error.failover_policy(), FailoverPolicy::Failover);
}

#[test]
fn too_many_requests_maps_to_rate_limit_or_concurrency() {
    let plain = map_http_status(429, Some("7"), r#"{"err_msg":"Rate limit exceeded."}"#);
    assert_eq!(plain.class, SttErrorClass::RateLimit);
    assert_eq!(plain.retry_after_ms, Some(7_000), "Retry-After is honoured");
    assert_eq!(plain.failover_policy(), FailoverPolicy::Backoff);

    // Deepgram counts concurrency per *project*, not per key (spec 7.5), and
    // the two-stream default burns two. It arrives as a 429 but is a different
    // fact: the supervisor can degrade to single mixed mono rather than just
    // wait for a limit that is not going to clear.
    let concurrent = map_http_status(
        429,
        None,
        r#"{"err_msg":"Maximum number of concurrent streams reached for this project."}"#,
    );
    assert_eq!(concurrent.class, SttErrorClass::Concurrency);
    assert_eq!(concurrent.failover_policy(), FailoverPolicy::Backoff);
    assert!(concurrent.retryable);
}

#[test]
fn bad_request_about_audio_is_classified_as_audio_format() {
    let error = map_http_status(400, None, r#"{"err_msg":"Unsupported encoding: linear24"}"#);
    assert_eq!(error.class, SttErrorClass::AudioFormat);
    assert_eq!(error.failover_policy(), FailoverPolicy::Surface);

    let other = map_http_status(400, None, r#"{"err_msg":"unknown parameter frobnicate"}"#);
    assert_eq!(other.class, SttErrorClass::BadRequest);
}

#[test]
fn server_errors_are_retryable_without_failing_over() {
    for status in [500u16, 502, 503, 504] {
        let error = map_http_status(status, None, "");
        assert_eq!(error.class, SttErrorClass::Server, "HTTP {status}");
        assert!(error.retryable);
        assert_eq!(error.failover_policy(), FailoverPolicy::Backoff);
    }
}

#[test]
fn close_1011_with_net_0001_is_a_retryable_network_error() {
    // §7.4's named failure: ten seconds without audio or a KeepAlive. The fix
    // is a new socket, so it must not be classified as a provider fault.
    let error = map_close(
        1011,
        "NET-0001 Deepgram did not receive audio data or a text message within the timeout window.",
    )
    .expect("1011 is not a clean close");

    assert_eq!(error.class, SttErrorClass::Network);
    assert!(error.retryable);
    assert_eq!(error.failover_policy(), FailoverPolicy::Backoff);
    assert!(error.counts_against_failure_budget());
}

#[test]
fn close_1011_without_a_deepgram_code_is_a_server_error() {
    let error = map_close(1011, "internal error").expect("1011 is not clean");
    assert_eq!(error.class, SttErrorClass::Server);
    assert!(error.retryable);
}

#[test]
fn clean_closes_are_not_errors() {
    assert!(map_close(1000, "").is_none());
    assert!(map_close(1001, "going away").is_none());
}

#[test]
fn abnormal_and_policy_closes_land_in_the_right_classes() {
    assert_eq!(
        map_close(1006, "").expect("abnormal").class,
        SttErrorClass::Network
    );
    assert_eq!(
        map_close(1008, "policy violation").expect("policy").class,
        SttErrorClass::BadRequest
    );
    assert_eq!(
        map_close(1013, "try again later").expect("try again").class,
        SttErrorClass::RateLimit
    );
}

#[test]
fn data_coded_closes_are_audio_format_problems() {
    let error = map_close(1008, "DATA-0000 Deepgram could not process the audio").expect("coded");
    assert_eq!(error.class, SttErrorClass::AudioFormat);
    assert_eq!(error.failover_policy(), FailoverPolicy::Surface);
}

#[test]
fn deepgram_codes_are_extracted_from_free_text() {
    assert_eq!(
        extract_deepgram_code("NET-0001 something happened"),
        Some("NET-0001".to_string())
    );
    assert_eq!(
        extract_deepgram_code("failed with (DATA-0000)."),
        Some("DATA-0000".to_string())
    );
    assert_eq!(extract_deepgram_code("no code here"), None);
    assert_eq!(
        extract_deepgram_code("NET-1"),
        None,
        "codes are four digits"
    );
}

#[test]
fn error_frames_on_the_socket_normalize_into_the_taxonomy() {
    let frame: DeepgramErrorFrame = serde_json::from_str(
        r#"{"type":"Error","description":"NET-0001 timeout","message":"Websocket error"}"#,
    )
    .expect("the error frame parses");
    assert!(frame.is_error());

    let error = frame.to_stt_error();
    assert_eq!(error.class, SttErrorClass::Network);
    assert!(error.retryable);
}

#[test]
fn a_results_frame_is_not_mistaken_for_an_error_frame() {
    let frame: DeepgramErrorFrame =
        serde_json::from_str(r#"{"type":"Results","is_final":true}"#).expect("parses");
    assert!(!frame.is_error());
}

#[test]
fn an_uncoded_error_frame_falls_back_to_server() {
    let frame: DeepgramErrorFrame =
        serde_json::from_str(r#"{"type":"Error","description":"something broke"}"#)
            .expect("parses");
    let error = frame.to_stt_error();
    assert_eq!(error.class, SttErrorClass::Server);
    assert!(error.retryable, "a 5xx-shaped failure is worth retrying");
}

// ---------------------------------------------------- frame shape, per type

/// `channel` is not one shape. On a `Results` frame it is the object holding
/// `alternatives`; on a `SpeechStarted` frame it is an array of channel
/// indices, `[0,1]`.
///
/// Typing it as the object alone meant serde read `[0,1]` as the struct,
/// mapped element `0` onto `alternatives`, and failed with "invalid type:
/// integer `0`, expected a sequence". Because `vad_events=true` is in the spec
/// parameter set, `SpeechStarted` is the FIRST frame Deepgram sends — so the
/// stream died before a single word was transcribed, on every meeting.
#[test]
fn a_speech_started_frame_does_not_break_the_reader() {
    let raw = r#"{"type":"SpeechStarted","channel":[0,1],"timestamp":0.05}"#;
    let parsed: fotw_stt::deepgram::DeepgramResult =
        serde_json::from_str(raw).expect("SpeechStarted must parse");

    assert_eq!(parsed.message_type.as_deref(), Some("SpeechStarted"));
    assert!(
        parsed.channel.is_none(),
        "an array channel carries no alternatives and must read as absent"
    );
}

#[test]
fn an_utterance_end_frame_parses_too() {
    let raw = r#"{"type":"UtteranceEnd","channel":[0,1],"last_word_end":2.5}"#;
    let parsed: fotw_stt::deepgram::DeepgramResult =
        serde_json::from_str(raw).expect("UtteranceEnd must parse");
    assert_eq!(parsed.message_type.as_deref(), Some("UtteranceEnd"));
}

/// The object form must still be read, or the fix would trade one silent
/// failure for another.
#[test]
fn a_results_frame_still_yields_its_alternatives() {
    let raw = r#"{"type":"Results","channel_index":[0,1],"duration":1.0,"start":0.0,
        "is_final":true,"speech_final":true,
        "channel":{"alternatives":[{"transcript":"hello there","confidence":0.99,"words":[]}]}}"#;
    let parsed: fotw_stt::deepgram::DeepgramResult =
        serde_json::from_str(raw).expect("Results must parse");

    let channel = parsed.channel.expect("Results carries a channel object");
    assert_eq!(
        channel.alternatives.first().map(|a| a.transcript.as_str()),
        Some("hello there")
    );
    assert!(parsed.is_final);
}
