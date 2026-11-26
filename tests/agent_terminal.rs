//! #113: terminal input rules and Codex id discovery. Pure; no PTY.
use murmur::agent::terminal::{
    Prompt, codex_thread_from_lsof, inherited_session_marker, paste_echoed, query_answers,
    voice_bytes,
};

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

#[test]
fn terminal_queries_get_answers_in_order() {
    assert_eq!(
        query_answers(b"x\x1b]11;?\x1b\\y\x1b[6n\x1b[c"),
        b"\x1b]11;rgb:0000/0000/0000\x1b\\\x1b[1;1R\x1b[?1;2c".to_vec()
    );
    assert_eq!(
        query_answers(b"\x1b]10;?\x07"),
        b"\x1b]10;rgb:ffff/ffff/ffff\x07".to_vec()
    );
    assert!(query_answers(b"plain output \x1b[31mred").is_empty());
}

#[test]
fn only_parent_session_markers_are_removed() {
    for name in [
        "CLAUDECODE",
        "CLAUDE_CODE_CHILD_SESSION",
        "CLAUDE_CODE_SESSION_ID",
        "CODEX_THREAD_ID",
    ] {
        assert!(inherited_session_marker(name), "{name}");
    }
    for name in [
        "CLAUDE_CODE_USE_BEDROCK",
        "ANTHROPIC_API_KEY",
        "PATH",
        "HOME",
    ] {
        assert!(!inherited_session_marker(name), "{name}");
    }
}

#[test]
fn a_paste_counts_as_echoed_only_when_the_input_box_shows_it() {
    let text = "run the full test suite please";
    // Redrawn with colors and a wrap in the middle.
    assert!(paste_echoed(
        b"\x1b[2K\x1b[38;5;12m> run the full te\r\n\x1b[0mst suite please",
        text
    ));
    // Long pastes are collapsed by the native TUIs.
    assert!(paste_echoed(b"> [Pasted text #1 +12 lines]", text));
    assert!(paste_echoed(b"> [Pasted Content 1400 chars]", text));
    // A dialog redraw without the text is not an echo.
    assert!(!paste_echoed(b"Allow tool? 1. Yes 2. No", text));
    assert!(!paste_echoed(b"", text));
}
