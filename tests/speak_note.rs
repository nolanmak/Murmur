//! A Control+Option dictation carries a short note asking the agent to
//! answer aloud with Murmur's speak tool.
use murmur::agent::speak_note;

#[test]
fn the_note_is_appended_on_one_line() {
    let text = speak_note("run the tests and tell me what failed");
    assert!(text.starts_with("run the tests and tell me what failed "));
    assert!(text.contains("speak tool"), "{text}");
    // Multi-line text is held for review instead of pasted; the note must
    // never turn a one-line dictation into that.
    assert!(!text.contains('\n'));
}

#[test]
fn the_note_is_not_doubled_or_added_to_empty_text() {
    let once = speak_note("hello");
    assert_eq!(speak_note(&once), once);
    assert_eq!(speak_note("   "), "");
}
