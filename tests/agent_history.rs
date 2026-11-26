//! #112: bounded, read-only native history previews. Synthetic JSONL only.
use murmur::agent::{
    Agent,
    history::{self, Message},
};
use std::path::Path;

const CODEX_ID: &str = "01a215a1-b6f1-77a3-be28-54e40869e9b2";
const CLAUDE_ID: &str = "3f0c6f3e-9a3b-4c55-8d1e-0b6a3c2d1e0f";

fn codex_lines() -> String {
    [
        r#"{"timestamp":"2026-11-26T02:14:10Z","type":"session_meta","payload":{"id":"x"}}"#,
        r#"{"timestamp":"2026-11-26T02:14:11Z","ordinal":1,"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"run the tests"}]}}"#,
        r#"{"timestamp":"2026-11-26T02:14:12Z","ordinal":2,"type":"response_item","payload":{"type":"message","role":"developer","content":[{"type":"input_text","text":"hidden"}]}}"#,
        r#"{"timestamp":"2026-11-26T02:14:13Z","ordinal":3,"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"All green."}]}}"#,
        "not json",
    ]
    .join("\n")
}

#[test]
fn codex_history_keeps_user_and_assistant_text_with_timestamps() {
    let messages = history::parse(codex_lines().as_bytes(), Agent::Codex, false);
    assert_eq!(
        messages,
        vec![
            Message {
                role: "user".into(),
                text: "run the tests".into(),
                timestamp: Some("2026-11-26T02:14:11Z".into()),
                id: Some("1".into()),
            },
            Message {
                role: "assistant".into(),
                text: "All green.".into(),
                timestamp: Some("2026-11-26T02:14:13Z".into()),
                id: Some("3".into()),
            },
        ]
    );
}

#[test]
fn claude_history_reads_string_and_block_content_and_skips_tools() {
    let lines = [
        r#"{"type":"user","uuid":"u1","timestamp":"t1","message":{"role":"user","content":"hello"}}"#,
        r#"{"type":"assistant","uuid":"a1","timestamp":"t2","message":{"role":"assistant","content":[{"type":"tool_use","name":"Bash"},{"type":"text","text":"Hi there."}]}}"#,
        r#"{"type":"user","uuid":"u2","timestamp":"t3","message":{"role":"user","content":[{"type":"tool_result","content":"secret output"}]}}"#,
    ]
    .join("\n");
    let messages = history::parse(lines.as_bytes(), Agent::Claude, false);
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[1].text, "Hi there.");
    assert_eq!(messages[1].id.as_deref(), Some("a1"));
}

#[test]
fn history_is_bounded_to_the_last_forty_messages_and_drops_a_cut_line() {
    let mut lines = vec!["{\"type\":\"user\",\"message\":{\"content\":\"cut".to_string()];
    for i in 0..60 {
        lines.push(format!(
            r#"{{"type":"user","uuid":"u{i}","message":{{"content":"m{i}"}}}}"#
        ));
    }
    let messages = history::parse(lines.join("\n").as_bytes(), Agent::Claude, true);
    assert_eq!(messages.len(), 40);
    assert_eq!(messages[0].text, "m20");
    assert_eq!(messages[39].text, "m59");
}

fn write(path: &Path, body: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, body).unwrap();
}

#[test]
fn native_files_are_located_by_uuid_only() {
    let home = tempfile::tempdir().unwrap();
    let codex = home.path().join(format!(
        ".codex/sessions/2026/11/25/rollout-2026-11-25T18-14-09-{CODEX_ID}.jsonl"
    ));
    write(&codex, &codex_lines());
    let claude = home
        .path()
        .join(format!(".claude/projects/-Users-me-proj/{CLAUDE_ID}.jsonl"));
    write(&claude, "");
    assert_eq!(
        history::locate(home.path(), Agent::Codex, CODEX_ID).unwrap(),
        Some(codex.clone())
    );
    assert_eq!(
        history::locate(home.path(), Agent::Claude, CLAUDE_ID).unwrap(),
        Some(claude)
    );
    assert_eq!(
        history::locate(home.path(), Agent::Claude, CODEX_ID).unwrap(),
        None
    );
    for bad in ["../../etc/passwd", "*", "", "01a215a1"] {
        assert!(
            history::locate(home.path(), Agent::Codex, bad).is_err(),
            "{bad}"
        );
    }
    let messages = history::read(&codex, Agent::Codex).unwrap();
    assert_eq!(messages.len(), 2);
}

#[cfg(unix)]
#[test]
fn symlinked_history_files_are_not_followed() {
    let home = tempfile::tempdir().unwrap();
    let outside = home.path().join("outside.jsonl");
    write(&outside, &codex_lines());
    let link = home
        .path()
        .join(format!(".claude/projects/-p/{CLAUDE_ID}.jsonl"));
    std::fs::create_dir_all(link.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&outside, &link).unwrap();
    assert_eq!(
        history::locate(home.path(), Agent::Claude, CLAUDE_ID).unwrap(),
        None
    );
}
