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

#[cfg(unix)]
fn agent_cmd(home: &std::path::Path, args: &[&str]) -> Command {
    let fake =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fake_agent.py");
    let mut command = Command::new(env!("CARGO_BIN_EXE_murmur"));
    command
        .args(args)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", home)
        .env("MURMUR_CLAUDE", &fake)
        .env("MURMUR_CODEX", &fake)
        .current_dir(home);
    command
}

#[cfg(unix)]
#[test]
fn agent_commands_without_a_daemon_explain_how_to_start_one() {
    let home = tempfile::tempdir().unwrap();
    for args in [&["agent", "list"][..], &["agent", "send", "hello"][..]] {
        let out = agent_cmd(home.path(), args).output().unwrap();
        assert!(!out.status.success());
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("murmur agent run"),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

#[cfg(unix)]
#[test]
fn agent_run_starts_the_daemon_and_detaching_keeps_the_session() {
    // Unix socket paths are limited to ~104 bytes; keep HOME shallow.
    let root = tempfile::Builder::new()
        .prefix("mh")
        .tempdir_in("/tmp")
        .unwrap();
    let home = root.path().join("h");
    std::fs::create_dir_all(&home).unwrap();
    /// Stops the spawned daemon even when an assertion fails.
    struct Stop(std::path::PathBuf);
    impl Drop for Stop {
        fn drop(&mut self) {
            let _ = agent_cmd(&self.0, &["agent", "stop"]).output();
        }
    }
    let _stop = Stop(home.clone());
    // Launched from inside another agent session: its identity markers must
    // not reach the new native session (Claude stops saving transcripts).
    let mut run = agent_cmd(&home, &["agent", "run", "claude"])
        .env("CLAUDECODE", "1")
        .env("CLAUDE_CODE_CHILD_SESSION", "1")
        .env("CLAUDE_CODE_SESSION_ID", "parent")
        .env("CLAUDE_PID", "1")
        .env("CODEX_THREAD_ID", "parent")
        .env("CLAUDE_CODE_USE_BEDROCK", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = run.stdin.take().unwrap();
    // Detach only once the agent is up (a fixed sleep flaked under load).
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::<u8>::new()));
    let mut pipe = run.stdout.take().unwrap();
    let sink = seen.clone();
    let reader = std::thread::spawn(move || {
        use std::io::Read;
        let mut chunk = [0u8; 4096];
        while let Ok(n) = pipe.read(&mut chunk) {
            if n == 0 {
                break;
            }
            sink.lock().unwrap().extend_from_slice(&chunk[..n]);
        }
    });
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    while !String::from_utf8_lossy(&seen.lock().unwrap()).contains("fake claude ready")
        && std::time::Instant::now() < deadline
    {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    stdin.write_all(&[0x1d]).unwrap(); // Ctrl+] detaches
    let out = run.wait_with_output().unwrap();
    reader.join().unwrap();
    let stdout = String::from_utf8_lossy(&seen.lock().unwrap()).to_string();
    assert!(
        out.status.success(),
        "{stdout} {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        stdout.contains("fake claude ready inherited=CLAUDE_CODE_USE_BEDROCK\r"),
        "{stdout}"
    );
    assert!(stdout.contains("Detached"), "{stdout}");

    let list = agent_cmd(&home, &["agent", "list"]).output().unwrap();
    let list = String::from_utf8_lossy(&list.stdout);
    assert!(list.contains("claude") && list.contains("live"), "{list}");

    let send = agent_cmd(&home, &["agent", "send", "hello", "there"])
        .output()
        .unwrap();
    assert!(
        send.status.success(),
        "{}",
        String::from_utf8_lossy(&send.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&send.stdout).trim(),
        "You said: hello there"
    );

    let stop = agent_cmd(&home, &["agent", "stop"]).output().unwrap();
    assert!(stop.status.success());
    let list = agent_cmd(&home, &["agent", "list"]).output().unwrap();
    assert!(!list.status.success());
}

#[cfg(unix)]
#[test]
fn agent_key_turns_the_right_option_hold_on_and_off() {
    let home = tempfile::tempdir().unwrap();
    let settings = home
        .path()
        .join("Library/Application Support/Murmur/agent-voice.json");
    let run = |args: &[&str]| {
        let out = agent_cmd(home.path(), args).output().unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).to_string()
    };
    let status = run(&["agent", "key", "status"]);
    assert!(
        status.contains("on") && status.contains("Control"),
        "{status}"
    );
    assert!(run(&["agent", "key", "off"]).contains("off"));
    assert!(!murmur::agent::voice::key_enabled(&settings));
    assert!(run(&["agent", "key", "on"]).contains("Right Option"));
    assert!(murmur::agent::voice::key_enabled(&settings));
    run(&["agent", "voice", "system"]);
    assert!(run(&["agent", "key", "status"]).contains("on"));
    run(&["agent", "key", "off"]);
    assert!(!murmur::agent::voice::key_enabled(&settings));
    assert_eq!(
        murmur::agent::speech::Settings::load(&settings)
            .unwrap()
            .voice,
        murmur::agent::speech::Voice::System
    );
    let bad = agent_cmd(home.path(), &["agent", "key", "maybe"])
        .output()
        .unwrap();
    assert!(!bad.status.success());
}
