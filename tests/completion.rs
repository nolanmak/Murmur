use murmur::core::{CompletionEffect, Dictation, Phase};

#[test]
fn a_duplicate_result_cannot_replace_a_completed_delivery_with_an_empty_notice() {
    let mut dictation = Dictation::default();
    assert!(dictation.start_manual());
    let generation = dictation.generation();
    assert!(dictation.stop());

    assert_eq!(
        dictation.complete(generation, Ok("Café 👋".into())),
        Some(CompletionEffect::Transcript("Café 👋".into()))
    );
    assert_eq!(dictation.phase, Phase::Idle);
    assert_eq!(dictation.complete(generation, Ok("late".into())), None);
    assert_eq!(
        dictation.complete(generation, Err("late error".into())),
        None
    );
}

#[test]
fn cancelled_results_are_ignored_and_a_new_attempt_can_finish() {
    let mut dictation = Dictation::default();
    assert!(dictation.start_manual());
    let cancelled = dictation.generation();
    assert!(dictation.cancel());
    assert_eq!(dictation.phase, Phase::Idle);
    assert_eq!(dictation.complete(cancelled, Ok("stale".into())), None);

    assert!(dictation.start_manual());
    let next = dictation.generation();
    assert!(dictation.stop());
    assert_eq!(
        dictation.complete(next, Ok("next".into())),
        Some(CompletionEffect::Transcript("next".into()))
    );
    assert_eq!(dictation.phase, Phase::Idle);
}

#[test]
fn an_empty_result_returns_to_idle_and_does_not_block_the_next_attempt() {
    let mut dictation = Dictation::default();
    assert!(dictation.start_manual());
    let empty = dictation.generation();
    assert!(dictation.stop());
    assert_eq!(
        dictation.complete(empty, Ok("  ".into())),
        Some(CompletionEffect::Empty)
    );
    assert_eq!(dictation.phase, Phase::Idle);

    assert!(dictation.start_manual());
    let next = dictation.generation();
    assert!(dictation.stop());
    assert_eq!(
        dictation.complete(next, Err("test failure".into())),
        Some(CompletionEffect::Error("test failure".into()))
    );
    assert_eq!(dictation.phase, Phase::Idle);
}

#[test]
fn a_worker_failure_during_recording_ends_the_attempt_instead_of_hanging() {
    // The worker can fail before release (mic stall, connection error). Dropping
    // that result left the attempt waiting forever once Control was released.
    let mut dictation = Dictation::default();
    assert_eq!(dictation.flags(true, false), Some("start"));
    let generation = dictation.generation();

    assert_eq!(
        dictation.complete(
            generation,
            Err("Microphone stopped delivering audio".into())
        ),
        Some(CompletionEffect::Error(
            "Microphone stopped delivering audio".into()
        ))
    );
    assert_eq!(dictation.phase, Phase::Idle);
    // Releasing Control afterwards neither re-enters processing nor restarts.
    assert_eq!(dictation.flags(false, false), None);
    assert_eq!(dictation.phase, Phase::Idle);
    assert_eq!(dictation.flags(true, false), Some("start"));
}

#[test]
fn text_that_arrives_before_release_is_never_inserted() {
    let mut dictation = Dictation::default();
    assert!(dictation.start_manual());
    let generation = dictation.generation();
    assert!(matches!(
        dictation.complete(generation, Ok("too early".into())),
        Some(CompletionEffect::Error(_))
    ));
    assert_eq!(dictation.phase, Phase::Idle);
}

#[test]
fn an_early_result_from_a_cancelled_attempt_cannot_end_the_new_one() {
    let mut dictation = Dictation::default();
    assert!(dictation.start_manual());
    let stale = dictation.generation();
    assert!(dictation.cancel());
    assert!(dictation.start_manual());
    assert_eq!(dictation.complete(stale, Err("stale".into())), None);
    assert_eq!(dictation.phase, Phase::Recording);
}

#[test]
fn a_panicking_worker_still_reports_an_error() {
    let result = murmur::core::contain_worker(|| -> Result<String, String> {
        panic!("boom");
    });
    assert!(result.unwrap_err().contains("no text inserted"));
    assert_eq!(
        murmur::core::contain_worker(|| Ok("fine".into())),
        Ok("fine".to_string())
    );
}

#[test]
fn processing_backstop_outlasts_every_worker_deadline() {
    use std::time::Duration;
    // Local inference (180 s) after a cloud finalization wait (3 s) is the
    // longest bounded path; the backstop must never pre-empt it.
    assert!(murmur::core::PROCESSING_BACKSTOP > Duration::from_secs(183));
    assert!(murmur::core::processing_overdue(
        murmur::core::PROCESSING_BACKSTOP
    ));
    assert!(!murmur::core::processing_overdue(Duration::from_secs(183)));
}
