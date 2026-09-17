use text_to_speech::core::Dictation;
#[test]
fn fn_hold_release_is_one_session_despite_repeated_flags_events() {
    let mut d = Dictation::default();
    assert_eq!(d.flags(true, false), Some("start"));
    assert_eq!(d.flags(true, false), None);
    assert_eq!(d.flags(false, false), Some("finish"));
    assert_eq!(d.flags(false, false), None);
}
#[test]
fn escape_cancels_once_and_fn_release_does_not_commit() {
    let mut d = Dictation::default();
    d.flags(true, false);
    assert!(d.cancel());
    assert!(!d.cancel());
    assert_eq!(d.flags(false, false), None);
}
#[test]
fn fn_chords_do_not_start_and_new_recording_waits_for_processing() {
    let mut d = Dictation::default();
    assert_eq!(d.flags(true, true), None);
    d.flags(false, false);
    assert_eq!(d.flags(true, false), Some("start"));
    assert_eq!(d.flags(false, false), Some("finish"));
    assert_eq!(d.flags(true, false), None);
    d.finish();
    d.flags(false, false);
    assert_eq!(d.flags(true, false), Some("start"));
}

#[test]
fn cancelled_generation_cannot_insert_after_a_new_session_starts() {
    let mut d = Dictation::default();
    d.flags(true, false);
    let stale = d.generation();
    d.flags(false, false);
    assert!(d.accepts(stale));
    d.cancel();
    d.flags(true, false);
    d.flags(false, false);
    assert!(!d.accepts(stale));
    assert!(d.accepts(d.generation()));
}
#[test]
fn pressing_a_modifier_during_dictation_cancels_the_chord() {
    let mut d = Dictation::default();
    d.flags(true, false);
    assert_eq!(d.flags(true, true), Some("cancel"));
    assert_eq!(d.flags(false, false), None);
}
