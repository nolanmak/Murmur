//! #112: CLI entry points, through the built binary.
use std::{
    io::Write,
    process::{Command, Stdio},
};

fn murmur(args: &[&str], stdin: &str, home: &std::path::Path) -> std::process::Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_murmur"))
        .args(args)
        .env("HOME", home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

#[test]
fn mcp_serves_line_delimited_json_rpc_over_stdio() {
    let home = tempfile::tempdir().unwrap();
    let out = murmur(
        &["mcp"],
        "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/list\"}\n{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/call\",\"params\":{\"name\":\"session_history\",\"arguments\":{\"agent\":\"codex\",\"id\":\"01a215a1-b6f1-77a3-be28-54e40869e9b2\"}}}\n",
        home.path(),
    );
    assert!(out.status.success());
    let lines: Vec<serde_json::Value> = String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0]["result"]["tools"][0]["name"], "speak");
    let history: serde_json::Value =
        serde_json::from_str(lines[1]["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(history["messages"], serde_json::json!([]));
}

#[test]
fn speak_refuses_bad_input_before_playback() {
    let home = tempfile::tempdir().unwrap();
    let out = murmur(&["speak", "--speed", "3", "--text", "hi"], "", home.path());
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("speed"));
    let out = murmur(&["speak"], "   ", home.path());
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("1–12000"));
}

#[test]
fn agent_voice_settings_are_saved_under_application_support() {
    let home = tempfile::tempdir().unwrap();
    let out = murmur(&["agent", "voice", "system"], "", home.path());
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stdout).contains("system"));
    let saved = std::fs::read_to_string(
        home.path()
            .join("Library/Application Support/Murmur/agent-voice.json"),
    )
    .unwrap();
    assert!(saved.contains("\"system\""));
}

#[test]
fn removed_session_commands_are_gone() {
    // The separate-session agent (agentd, agent run/send/…) was removed:
    // voice goes to the session you are in (Control+Option dictation).
    let home = tempfile::tempdir().unwrap();
    for args in [
        &["agent", "run", "claude"][..],
        &["agent", "send", "hi"][..],
        &["agentd"][..],
    ] {
        let out = murmur(args, "", home.path());
        assert!(!out.status.success(), "{args:?}");
    }
}
