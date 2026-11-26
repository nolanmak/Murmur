//! #113: Murmur-owned native sessions, end to end through the Unix socket
//! with a synthetic agent CLI (tests/fixtures/fake_agent.py).
#![cfg(unix)]
use murmur::agent::{
    Agent,
    client::{self, Frame},
    daemon::{self, Config},
};
use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fake_agent.py")
}

struct Env {
    _root: tempfile::TempDir,
    dir: PathBuf,
    project: PathBuf,
    daemon: daemon::Handle,
}

fn start() -> Env {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    let project = root.path().join("project");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&project).unwrap();
    // Short socket path: Unix socket paths are limited to ~104 bytes.
    let dir = root.path().join("s");
    let daemon = daemon::start(Config {
        dir: dir.clone(),
        home,
        codex: fixture(),
        claude: fixture(),
        poll: Duration::from_millis(50),
        deadline: Duration::from_secs(20),
    })
    .unwrap();
    Env {
        _root: root,
        dir,
        project,
        daemon,
    }
}

/// Reads frames until `needle` appears in the output or the deadline passes.
fn wait_for(attach: &mut client::Attachment, needle: &str) -> String {
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut seen = String::new();
    while Instant::now() < deadline {
        match attach.read(Duration::from_millis(200)).unwrap() {
            Some(Frame::Output(bytes)) => seen.push_str(&String::from_utf8_lossy(&bytes)),
            Some(Frame::Exit(_)) => break,
            None => {}
        }
        if seen.contains(needle) {
            return seen;
        }
    }
    panic!("{needle:?} not seen in {seen:?}");
}

#[test]
fn socket_directory_and_socket_are_private() {
    use std::os::unix::fs::PermissionsExt;
    let env = start();
    let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&env.dir), 0o700);
    assert_eq!(mode(&env.dir.join(daemon::SOCKET)), 0o600);
    assert!(client::list(&env.dir).unwrap().is_empty());
    env.daemon.shutdown();
}

#[test]
fn claude_session_survives_detach_and_answers_a_voice_turn() {
    let env = start();
    let id = client::open(&env.dir, Agent::Claude, None, &env.project, 100, 30).unwrap();
    assert!(murmur::agent::history::is_uuid(&id), "{id}");
    let mut attach = client::attach(&env.dir, &id, 100, 30).unwrap();
    wait_for(&mut attach, "fake claude ready");
    drop(attach); // detach
    std::thread::sleep(Duration::from_millis(200));
    let sessions = client::list(&env.dir).unwrap();
    assert_eq!(sessions.len(), 1);
    assert!(sessions[0].live && sessions[0].active);
    // Reattaching replays recent output.
    let mut attach = client::attach(&env.dir, &id, 100, 30).unwrap();
    wait_for(&mut attach, "fake claude ready");
    drop(attach);

    let mut streamed = Vec::new();
    let reply = client::send(&env.dir, None, "hello there", |text| {
        streamed.push(text.to_string())
    })
    .unwrap();
    assert_eq!(reply.trim(), "You said: hello there");
    assert_eq!(streamed, vec!["You said: hello there".to_string()]);
    env.daemon.shutdown();
}

#[test]
fn pending_typed_text_and_control_characters_block_voice() {
    let env = start();
    let id = client::open(&env.dir, Agent::Claude, None, &env.project, 100, 30).unwrap();
    let mut attach = client::attach(&env.dir, &id, 100, 30).unwrap();
    wait_for(&mut attach, "fake claude ready");
    attach.input(b"half typed").unwrap();
    std::thread::sleep(Duration::from_millis(200));
    let error = client::send(&env.dir, Some(&id), "spoken", |_| {}).unwrap_err();
    assert!(error.contains("unfinished text"), "{error}");
    attach.input(&[0x15]).unwrap(); // Ctrl+U clears it
    std::thread::sleep(Duration::from_millis(200));
    let error = client::send(&env.dir, Some(&id), "bad\x07", |_| {}).unwrap_err();
    assert!(error.contains("control"), "{error}");
    drop(attach);
    assert!(client::send(&env.dir, Some(&id), "fine now", |_| {}).is_ok());
    env.daemon.shutdown();
}

#[test]
fn codex_provisional_id_is_replaced_by_its_thread_id() {
    let env = start();
    let provisional = client::open(&env.dir, Agent::Codex, None, &env.project, 100, 30).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    let real = loop {
        let sessions = client::list(&env.dir).unwrap();
        if let Some(s) = sessions.iter().find(|s| s.id != provisional) {
            break s.id.clone();
        }
        assert!(Instant::now() < deadline, "codex id never resolved");
        std::thread::sleep(Duration::from_millis(100));
    };
    assert!(murmur::agent::history::is_uuid(&real));
    // Both ids address the same live session.
    let reply = client::send(&env.dir, Some(&provisional), "ping", |_| {}).unwrap();
    assert_eq!(reply.trim(), "Codex heard: ping");
    let reply = client::send(&env.dir, Some(&real), "again", |_| {}).unwrap();
    assert_eq!(reply.trim(), "Codex heard: again");
    env.daemon.shutdown();
}

#[test]
fn an_exited_session_fails_explicitly_and_unknown_ids_are_refused() {
    let env = start();
    let id = client::open(&env.dir, Agent::Claude, None, &env.project, 100, 30).unwrap();
    let mut attach = client::attach(&env.dir, &id, 100, 30).unwrap();
    wait_for(&mut attach, "fake claude ready");
    attach.input(b"exit\r").unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(Frame::Exit(_)) = attach.read(Duration::from_millis(200)).unwrap() {
            break;
        }
        assert!(Instant::now() < deadline, "no exit frame");
    }
    let error = client::send(&env.dir, Some(&id), "anyone?", |_| {}).unwrap_err();
    assert!(error.contains("exited"), "{error}");
    let error = client::send(&env.dir, None, "anyone?", |_| {}).unwrap_err();
    assert!(error.contains("murmur agent run"), "{error}");
    assert!(
        client::send(
            &env.dir,
            Some("3f0c6f3e-9a3b-4c55-8d1e-0b6a3c2d1e0f"),
            "x",
            |_| {}
        )
        .is_err()
    );
    assert!(client::open(&env.dir, Agent::Claude, None, Path::new("relative"), 80, 24).is_err());
    env.daemon.shutdown();
}
