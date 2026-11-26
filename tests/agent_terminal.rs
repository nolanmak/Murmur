//! #113: terminal input rules and Codex id discovery. Pure; no PTY.
use murmur::agent::terminal::{Prompt, codex_thread_from_lsof, voice_bytes};

#[test]
fn typed_text_marks_a_draft_until_enter_or_clear() {
    let mut p = Prompt::default();
    assert!(!p.draft());
    p.typed(b"half a thought");
    assert!(p.draft());
    p.typed(b"\r");
    assert!(!p.draft());
    p.typed(b"x");
    p.typed(&[0x15]); // Ctrl+U clears the line
    assert!(!p.draft());
    p.typed(b"y");
    p.typed(&[0x03]);
    assert!(!p.draft());
    // Arrow keys and other escape sequences are not drafts.
    p.typed(b"\x1b[A");
    assert!(!p.draft());
}

#[test]
fn voice_is_a_bracketed_paste_and_refuses_control_characters() {
    assert_eq!(
        voice_bytes("run the tests").unwrap(),
        b"\x1b[200~run the tests\x1b[201~".to_vec()
    );
    assert_eq!(
        voice_bytes("line one\nline two").unwrap(),
        b"\x1b[200~line one\nline two\x1b[201~".to_vec()
    );
    for bad in ["\x1b[201~escape", "bell\x07", "nul\0", "  "] {
        assert!(voice_bytes(bad).is_err(), "{bad:?}");
    }
}

#[test]
fn codex_thread_id_comes_from_the_writer_lock_it_holds() {
    let lsof = "p123\nfcwd\nn/Users/me/proj\nf27\nn/Users/me/.codex/thread-writer-locks/01a215a1-b6f1-77a3-be28-54e40869e9b2.lock\n";
    assert_eq!(
        codex_thread_from_lsof(lsof).as_deref(),
        Some("01a215a1-b6f1-77a3-be28-54e40869e9b2")
    );
    assert_eq!(codex_thread_from_lsof("p123\nn/tmp/x.lock\n"), None);
    assert_eq!(
        codex_thread_from_lsof("n/x/thread-writer-locks/../../evil.lock\n"),
        None
    );
}
