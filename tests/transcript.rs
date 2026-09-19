use murmur::transcript::Transcript;

#[test]
fn starting_a_new_attempt_clears_the_previous_recovery_transcript() {
    let mut latest = murmur::transcript::Latest::from("old remote transcript");
    latest.begin();
    assert!(latest.is_empty());
}

#[test]
fn a_completed_attempt_replaces_the_previous_recovery_transcript() {
    let mut latest = murmur::transcript::Latest::from("old remote transcript");
    latest.begin();
    latest.set("new transcript");
    assert_eq!(latest.as_ref(), "new transcript");
}
#[test]
fn partial_revisions_are_replaced_and_finals_are_not_duplicated() {
    let mut t = Transcript::default();
    t.accept("a", 0, "hello", false);
    t.accept("a", 1, "Hello world.", true);
    t.accept("a", 1, "Hello world.", true);
    t.accept("a", 0, "old", false);
    t.accept("b", 0, "Next sentence.", true);
    assert_eq!(t.text(), "Hello world. Next sentence.");
}
#[test]
fn incomplete_words_are_not_committed() {
    let mut t = Transcript::default();
    t.accept("a", 0, "never commit me", false);
    assert_eq!(t.text(), "");
}
