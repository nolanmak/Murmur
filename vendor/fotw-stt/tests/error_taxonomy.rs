//! The shared error taxonomy and its failover policy (spec 4.2 STT-12).
//!
//! > Shared error taxonomy across adapters (`auth`/`quota`/`rate_limit`/
//! > `concurrency`/`bad_request`/`unsupported`/`network`/`server`/
//! > `audio_format`/`session_limit`). **Only `auth` and `quota` trigger
//! > failover.**
//!
//! The policy is a correctness rule, not a heuristic. Deepgram's concurrency
//! limits are per *project*, not per key (spec 7.5), and the default two-stream
//! capture mode consumes two of them per meeting — so a user in back-to-back
//! calls can hit 429 with nothing whatsoever wrong. Failing the provider over on
//! that would demote a working provider for a condition that clears in seconds.

use fotw_stt::{FailoverPolicy, SttError, SttErrorClass};

/// Every class, so a class added later cannot skip these tests.
const ALL_CLASSES: [SttErrorClass; 10] = [
    SttErrorClass::Auth,
    SttErrorClass::Quota,
    SttErrorClass::RateLimit,
    SttErrorClass::Concurrency,
    SttErrorClass::BadRequest,
    SttErrorClass::Unsupported,
    SttErrorClass::Network,
    SttErrorClass::Server,
    SttErrorClass::AudioFormat,
    SttErrorClass::SessionLimit,
];

#[test]
fn the_taxonomy_is_exactly_the_ten_classes_in_the_spec() {
    let tags: Vec<String> = ALL_CLASSES
        .iter()
        .map(|class| {
            serde_json::to_value(class)
                .unwrap()
                .as_str()
                .unwrap()
                .to_string()
        })
        .collect();

    assert_eq!(
        tags,
        vec![
            "auth",
            "quota",
            "rate_limit",
            "concurrency",
            "bad_request",
            "unsupported",
            "network",
            "server",
            "audio_format",
            "session_limit",
        ]
    );
    assert_eq!(SttErrorClass::ALL, ALL_CLASSES);
}

#[test]
fn only_auth_and_quota_trigger_failover() {
    // The load-bearing assertion of STT-12, stated as an exhaustive partition so
    // adding a class without deciding its policy fails here.
    let failing_over: Vec<SttErrorClass> = ALL_CLASSES
        .into_iter()
        .filter(|class| class.failover_policy() == FailoverPolicy::Failover)
        .collect();

    assert_eq!(
        failing_over,
        vec![SttErrorClass::Auth, SttErrorClass::Quota],
        "exactly two classes may demote a provider"
    );
}

#[test]
fn rate_limit_and_concurrency_back_off_rather_than_failing_over() {
    // Deepgram's 150-concurrent-stream limit is per project and not raisable,
    // and the two-stream default burns two per meeting. Treating that as "this
    // provider is broken" would demote a perfectly healthy provider because the
    // user joined a second call.
    assert_eq!(
        SttErrorClass::RateLimit.failover_policy(),
        FailoverPolicy::Backoff
    );
    assert_eq!(
        SttErrorClass::Concurrency.failover_policy(),
        FailoverPolicy::Backoff
    );

    assert!(SttErrorClass::RateLimit.is_retryable());
    assert!(SttErrorClass::Concurrency.is_retryable());
}

#[test]
fn transient_transport_errors_back_off_and_terminal_request_errors_do_not() {
    // Retry the same provider.
    assert_eq!(
        SttErrorClass::Network.failover_policy(),
        FailoverPolicy::Backoff
    );
    assert_eq!(
        SttErrorClass::Server.failover_policy(),
        FailoverPolicy::Backoff
    );

    // Our bug or our audio. Retrying and failing over are both wrong: every
    // provider would reject it identically, and a silent demotion would hide the
    // defect.
    for class in [
        SttErrorClass::BadRequest,
        SttErrorClass::Unsupported,
        SttErrorClass::AudioFormat,
    ] {
        assert_eq!(class.failover_policy(), FailoverPolicy::Surface);
        assert!(!class.is_retryable());
    }
}

#[test]
fn a_session_limit_reconnects_transparently_and_costs_no_failure_budget() {
    // ElevenLabs' `session_time_limit_exceeded` is routine, not a fault — spec
    // 7.4 says to assume long meetings will hit it. Counting it against the
    // reconnect budget would demote the provider on schedule during any long
    // meeting.
    assert_eq!(
        SttErrorClass::SessionLimit.failover_policy(),
        FailoverPolicy::Reconnect
    );
    assert!(SttErrorClass::SessionLimit.is_retryable());
    assert!(!SttErrorClass::SessionLimit.counts_against_failure_budget());

    // Everything else that retries does count, or a flapping provider never gets
    // demoted.
    assert!(SttErrorClass::Network.counts_against_failure_budget());
    assert!(SttErrorClass::Server.counts_against_failure_budget());
}

#[test]
fn auth_and_quota_are_not_retryable() {
    // Retrying a bad key or an exhausted balance just burns the meeting.
    assert!(!SttErrorClass::Auth.is_retryable());
    assert!(!SttErrorClass::Quota.is_retryable());
}

#[test]
fn every_class_has_a_user_facing_hint_that_does_not_leak_provider_jargon() {
    for class in ALL_CLASSES {
        let hint = class.user_hint();
        assert!(!hint.is_empty(), "{class:?} has no hint");
        assert!(
            hint.chars().next().unwrap().is_uppercase(),
            "{class:?} hint is not a sentence: {hint}"
        );
    }
}

#[test]
fn an_error_carries_its_provider_message_and_optional_retry_after() {
    let error = SttError::new(
        SttErrorClass::RateLimit,
        "deepgram",
        "Too many requests for this project.",
    )
    .with_retry_after_ms(2_500);

    assert_eq!(error.class, SttErrorClass::RateLimit);
    assert_eq!(error.provider, "deepgram");
    assert_eq!(error.message, "Too many requests for this project.");
    assert_eq!(error.retry_after_ms, Some(2_500));
    assert!(error.retryable);
    assert_eq!(error.failover_policy(), FailoverPolicy::Backoff);

    // Defaults to none, so "wait exactly this long" is never invented.
    let bare = SttError::new(SttErrorClass::Network, "deepgram", "Connection reset.");
    assert_eq!(bare.retry_after_ms, None);
}

#[test]
fn retryable_is_derived_from_the_class_but_can_be_narrowed_by_an_adapter() {
    // Some 500s are permanent (a retired model id). An adapter may say so, but
    // it may never widen a non-retryable class into a retryable one, and it can
    // never change the failover policy.
    let permanent = SttError::new(SttErrorClass::Server, "openai", "Model retired.")
        .not_retryable("the model id no longer exists");

    assert!(!permanent.retryable);
    assert_eq!(permanent.failover_policy(), FailoverPolicy::Backoff);
    assert_eq!(
        permanent.detail.as_deref(),
        Some("the model id no longer exists")
    );
}

#[test]
fn an_error_displays_as_its_user_facing_message() {
    let error = SttError::new(
        SttErrorClass::Auth,
        "deepgram",
        "Your Deepgram key was rejected.",
    );

    assert_eq!(
        error.to_string(),
        "deepgram: Your Deepgram key was rejected."
    );
    // It is a real std::error::Error, so `?` works across the adapter boundary.
    let as_std: &dyn std::error::Error = &error;
    assert!(as_std.to_string().contains("deepgram"));
}

#[test]
fn errors_round_trip_over_the_wire() {
    // The daemon ships these to the browser over the WebSocket.
    let error = SttError::new(SttErrorClass::Concurrency, "deepgram", "Project busy.")
        .with_retry_after_ms(1_000);

    let json = serde_json::to_value(&error).expect("serializes");
    assert_eq!(
        json,
        serde_json::json!({
            "class": "concurrency",
            "provider": "deepgram",
            "message": "Project busy.",
            "retryable": true,
            "retryAfterMs": 1_000,
            "detail": serde_json::Value::Null,
        })
    );

    let back: SttError = serde_json::from_value(json).expect("round-trips");
    assert_eq!(back, error);
}

#[test]
fn failover_policy_on_the_error_agrees_with_the_class() {
    for class in ALL_CLASSES {
        let error = SttError::new(class, "deepgram", "boom");
        assert_eq!(error.failover_policy(), class.failover_policy());
        assert_eq!(error.retryable, class.is_retryable());
    }
}
