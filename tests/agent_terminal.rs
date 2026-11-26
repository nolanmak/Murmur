//! #113: terminal input rules and Codex id discovery. Pure; no PTY.
use murmur::agent::terminal::{
    Prompt, codex_thread_from_lsof, inherited_session_marker, query_answers, voice_bytes,
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
    // Cursor movement and other escape sequences are not drafts.
    p.typed(b"\x1b[C");
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

mod screen {
    use murmur::agent::terminal::Screen;

    fn screen(bytes: &[u8]) -> Screen {
        let mut s = Screen::new(10, 60);
        s.process(bytes);
        s
    }

    #[test]
    fn a_new_input_row_with_the_text_counts_as_echo() {
        let mut s = screen(b"Claude Code\r\n\r\n> ");
        let before = s.rows();
        s.process(b"yes please");
        assert!(Screen::newly_shows(&before, &s.rows(), "yes please"));
    }

    #[test]
    fn a_dialog_repaint_that_already_showed_the_word_is_not_an_echo() {
        // An approval dialog offering "1. Yes", repainted after the paste.
        let dialog = b"\x1b[H\x1b[2JAllow Bash?\r\n> 1. Yes\r\n  2. No\r\n";
        let mut s = screen(dialog);
        let before = s.rows();
        s.process(dialog);
        assert!(!Screen::newly_shows(&before, &s.rows(), "yes"));
        assert!(!Screen::newly_shows(&before, &s.rows(), "1"));
    }

    #[test]
    fn text_already_in_the_transcript_does_not_count_again() {
        let mut s = screen(b"> continue\r\nWorking...\r\n> ");
        let before = s.rows();
        s.process(b"\x1b[2J\x1b[H> continue\r\nWorking...\r\n> ");
        assert!(!Screen::newly_shows(&before, &s.rows(), "continue"));
        s.process(b"continue");
        assert!(Screen::newly_shows(&before, &s.rows(), "continue"));
    }

    #[test]
    fn a_collapsed_paste_placeholder_counts_only_when_new() {
        let mut s = screen(b"> ");
        let before = s.rows();
        s.process(b"[Pasted text #1 +12 lines]");
        assert!(Screen::newly_shows(
            &before,
            &s.rows(),
            &"long words ".repeat(200)
        ));
        let before = s.rows();
        assert!(!Screen::newly_shows(
            &before,
            &s.rows(),
            &"long words ".repeat(200)
        ));
    }
}

#[test]
fn pastes_history_recall_and_soft_newlines_are_drafts() {
    let mut p = Prompt::default();
    p.typed(b"\x1b[200~pasted\x1b[201~");
    assert!(p.draft(), "terminal paste");
    p.typed(&[0x15]);
    p.typed(b"\x1b[A");
    assert!(p.draft(), "history recall");
    p.typed(&[0x03]);
    p.typed(b"\x1bOA");
    assert!(p.draft(), "history recall (application mode)");
    p.typed(&[0x15]);
    p.typed(b"line one\\");
    p.typed(b"\r");
    assert!(p.draft(), "backslash-Enter inserts a newline");
    p.typed(b"\x1b\r");
    assert!(p.draft(), "Option+Enter inserts a newline");
    p.typed(b"\r");
    assert!(!p.draft());
    p.typed(&[0x7f]);
    assert!(!p.draft(), "Backspace on an empty prompt");
    p.mark_draft();
    assert!(p.draft());
}

#[test]
fn attach_replay_drops_old_terminal_queries_and_a_cut_first_line() {
    use murmur::agent::terminal::replay;
    assert_eq!(
        replay(b"hello\x1b[6n world\x1b]11;?\x1b\\!\x1b[c", false),
        b"hello world!".to_vec()
    );
    assert_eq!(
        replay(b"[31mcut\r\nwhole line", true),
        b"whole line".to_vec()
    );
}
