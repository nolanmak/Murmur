//! #114: dictation routed to the active agent with a spoken reply. Uses the
//! synthetic agent CLI and a fake player; no microphone, key or audio.
#![cfg(unix)]
use murmur::agent::{
    Agent, client, daemon,
    speech::{Player, Settings, Voice, Voicebox},
    voice::{self, Stage},
};
use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

#[test]
fn stage_messages_never_contain_transcript_or_reply_text() {
    for stage in [
        Stage::Sending,
        Stage::Waiting,
        Stage::Speaking,
        Stage::Replied("macOS voice"),
        Stage::Stopped,
    ] {
        let message = stage.message().unwrap_or_default();
        assert!(!message.contains("secret words"), "{message}");
    }
    assert_eq!(Stage::Stopped.message(), None);
}

#[test]
fn a_superseded_turn_is_silent_not_an_error() {
    let (_root, dir, project, daemon) = setup();
    let project = project.join("silent");
    std::fs::create_dir_all(&project).unwrap();
    client::open(&dir, Agent::Claude, None, &project, 100, 30).unwrap();
    std::thread::sleep(Duration::from_millis(500));
    let first_dir = dir.clone();
    let first = std::thread::spawn(move || {
        let mut stages = Vec::new();
        voice::deliver(
            &first_dir,
            "think about it",
            &offline_voice(),
            &FakePlayer::default(),
            |s| stages.push(s),
        );
        stages
    });
    std::thread::sleep(Duration::from_millis(1000));
    voice::deliver(
        &dir,
        "answer now",
        &offline_voice(),
        &FakePlayer::default(),
        |_| {},
    );
    assert_eq!(first.join().unwrap().last(), Some(&Stage::Stopped));
    daemon.shutdown();
}

#[derive(Default, Clone)]
struct FakePlayer {
    played: Arc<Mutex<Vec<(PathBuf, String)>>>,
}
impl Player for FakePlayer {
    fn play(&self, program: &Path, _args: &[String], file: &Path) -> Result<(), String> {
        self.played.lock().unwrap().push((
            program.into(),
            std::fs::read_to_string(file).unwrap_or_default(),
        ));
        Ok(())
    }
}

fn offline_voice() -> Voicebox {
    Voicebox::new(
        Box::new(|| {
            Ok((
                Settings {
                    voice: Voice::System,
                    ..Default::default()
                },
                false,
            ))
        }),
        Box::new(|| panic!("the offline voice never reads a key")),
        "http://127.0.0.1:9".into(),
        Box::new(FakePlayer::default()),
    )
}

fn setup() -> (tempfile::TempDir, PathBuf, PathBuf, daemon::Handle) {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    let project = root.path().join("project");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&project).unwrap();
    let dir = root.path().join("s");
    let fake = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fake_agent.py");
    let handle = daemon::start(daemon::Config {
        dir: dir.clone(),
        home,
        codex: fake.clone(),
        claude: fake,
        poll: Duration::from_millis(50),
        deadline: Duration::from_secs(20),
    })
    .unwrap();
    (root, dir, project, handle)
}

#[test]
fn a_dictation_reaches_the_active_session_and_the_reply_is_spoken() {
    let (_root, dir, project, daemon) = setup();
    client::open(&dir, Agent::Claude, None, &project, 100, 30).unwrap();
    std::thread::sleep(Duration::from_millis(500));
    let player = FakePlayer::default();
    let mut stages = Vec::new();
    voice::deliver(&dir, "secret words", &offline_voice(), &player, |s| {
        stages.push(s)
    });
    assert_eq!(
        stages,
        vec![
            Stage::Sending,
            Stage::Waiting,
            Stage::Speaking,
            Stage::Replied("macOS voice")
        ]
    );
    let played = player.played.lock().unwrap();
    assert_eq!(played.len(), 1);
    assert_eq!(played[0].0, PathBuf::from("/usr/bin/say"));
    assert_eq!(played[0].1, "You said: secret words");
    daemon.shutdown();
}

#[test]
fn without_a_session_nothing_is_sent_or_spoken_and_the_fix_is_named() {
    let (_root, dir, _project, daemon) = setup();
    let player = FakePlayer::default();
    let mut stages = Vec::new();
    voice::deliver(&dir, "secret words", &offline_voice(), &player, |s| {
        stages.push(s)
    });
    let Some(Stage::Failed(message)) = stages.last() else {
        panic!("{stages:?}");
    };
    assert!(message.contains("murmur agent run"), "{message}");
    assert!(!message.contains("secret words"));
    assert!(player.played.lock().unwrap().is_empty());
    daemon.shutdown();
    // And with no daemon at all.
    let mut stages = Vec::new();
    voice::deliver(&dir, "secret words", &offline_voice(), &player, |s| {
        stages.push(s)
    });
    assert!(
        matches!(stages.last(), Some(Stage::Failed(m)) if m.contains("murmur agent run")),
        "{stages:?}"
    );
}

#[test]
fn the_agent_key_is_on_by_default_and_persists() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("agent-voice.json");
    assert!(voice::key_enabled(&path));
    voice::set_key_enabled(&path, false).unwrap();
    assert!(!voice::key_enabled(&path));
    voice::set_key_enabled(&path, true).unwrap();
    assert!(voice::key_enabled(&path));
    assert_eq!(
        Settings::load(&path).unwrap().voice,
        Voice::Deepgram,
        "toggling the key keeps the voice choice"
    );
    // Files written by the dictation-route build load fine: the old route is
    // ignored (Control is never redirected) and the key keeps its default.
    std::fs::write(&path, br#"{"version":1,"route_dictation":true}"#).unwrap();
    assert!(voice::key_enabled(&path));
    std::fs::write(&path, br#"{"version":1,"agent_key":false}"#).unwrap();
    assert!(!voice::key_enabled(&path));
}
