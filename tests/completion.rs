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
    assert_eq!(dictation.complete(generation, Err("late error".into())), None);
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
