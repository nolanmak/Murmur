//! #112: spoken replies. Fake HTTP server and fake player only; no real key,
//! network or audio device.
use murmur::agent::speech::{self, Engine, Player, Settings, Voice};
use std::{
    io::{Read, Write},
    net::TcpListener,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

/// program, args, file bytes, file mode
type Call = (PathBuf, Vec<String>, Vec<u8>, u32);
#[derive(Default, Clone)]
struct FakePlayer {
    calls: Arc<Mutex<Vec<Call>>>,
}
impl Player for FakePlayer {
    fn play(&self, program: &Path, args: &[String], file: &Path) -> Result<(), String> {
        #[cfg(unix)]
        let mode = {
            use std::os::unix::fs::PermissionsExt;
            std::fs::metadata(file).unwrap().permissions().mode() & 0o777
        };
        #[cfg(not(unix))]
        let mode = 0o600;
        self.calls.lock().unwrap().push((
            program.to_path_buf(),
            args.to_vec(),
            std::fs::read(file).unwrap(),
            mode,
        ));
        Ok(())
    }
}

/// One-shot HTTP server; returns the URL and the raw request it received.
fn serve(responses: Vec<(u16, &'static str, Vec<u8>)>) -> (String, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    std::thread::spawn(move || {
        for (status, mime, body) in responses {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = Vec::new();
            let mut chunk = [0u8; 4096];
            loop {
                let n = stream.read(&mut chunk).unwrap();
                buf.extend_from_slice(&chunk[..n]);
                let text = String::from_utf8_lossy(&buf).to_string();
                if let Some(end) = text.find("\r\n\r\n") {
                    let length = text[..end]
                        .lines()
                        .find_map(|l| {
                            l.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .map(|v| v.trim().parse::<usize>().unwrap())
                        })
                        .unwrap_or(0);
                    if buf.len() >= end + 4 + length {
                        break;
                    }
                }
                if n == 0 {
                    break;
                }
            }
            log.lock()
                .unwrap()
                .push(String::from_utf8_lossy(&buf).to_string());
            let head = format!(
                "HTTP/1.1 {status} X\r\ncontent-type: {mime}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                body.len()
            );
            stream.write_all(head.as_bytes()).unwrap();
            stream.write_all(&body).unwrap();
        }
    });
    (url, seen)
}

fn deepgram(base: &str) -> Engine {
    Engine::Deepgram {
        base: base.into(),
        key: fotw_secrets::SecretString::new("fixture-token"),
        model: "aura-2-helena-en".into(),
    }
}

#[test]
fn text_and_speed_are_validated_before_any_work() {
    assert!(speech::validate_text("   ").is_err());
    assert!(speech::validate_text(&"a".repeat(12001)).is_err());
    assert_eq!(speech::validate_text("  hi  ").unwrap(), "hi");
    for bad in [0.5, 2.5, 1.1, f64::NAN] {
        assert!(speech::speed(bad).is_err(), "{bad}");
    }
    for good in [0.75, 1.0, 1.25, 1.5, 1.75, 2.0] {
        assert_eq!(speech::speed(good).unwrap(), good as f32);
    }
    let player = FakePlayer::default();
    let rt = tokio::runtime::Runtime::new().unwrap();
    // A refused request never reaches the (unbound) server or the player.
    assert!(
        rt.block_on(speech::speak(
            &deepgram("http://127.0.0.1:9"),
            &player,
            "",
            1.0
        ))
        .is_err()
    );
    assert!(player.calls.lock().unwrap().is_empty());
}

#[test]
fn long_text_is_chunked_at_sentence_boundaries_in_order() {
    let text = "First sentence here. Second one follows! Third? Fourth.";
    let parts = speech::chunks(text, 25);
    assert_eq!(parts.join(" "), text);
    assert!(parts.iter().all(|p| p.chars().count() <= 25), "{parts:?}");
    assert_eq!(parts[0], "First sentence here.");
    // A single run-on longer than the limit still splits, on whitespace.
    let runon = "word ".repeat(30);
    assert!(
        speech::chunks(&runon, 20)
            .iter()
            .all(|p| p.chars().count() <= 20)
    );
    assert_eq!(speech::chunks("short", 2000), vec!["short".to_string()]);
}

#[test]
fn deepgram_requests_aura_with_token_and_plays_each_chunk_privately() {
    let (url, seen) = serve(vec![
        (200, "audio/mpeg", b"mp3-one".to_vec()),
        (200, "application/octet-stream", b"mp3-two".to_vec()),
    ]);
    let player = FakePlayer::default();
    let text = format!("{}. Second.", "x".repeat(1895));
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(speech::speak(&deepgram(&url), &player, &text, 1.5))
        .unwrap();
    let requests = seen.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(
        requests[0].starts_with("POST /v1/speak?model=aura-2-helena-en&encoding=mp3 "),
        "{}",
        requests[0]
    );
    assert!(requests[0].contains("authorization: Token fixture-token"));
    assert!(requests[1].contains("\"text\":\"Second.\""));
    let calls = player.calls.lock().unwrap();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].0, PathBuf::from("/usr/bin/afplay"));
    assert_eq!(calls[0].1[..2], ["--rate".to_string(), "1.5".into()]);
    assert_eq!(calls[0].2, b"mp3-one");
    assert_eq!(calls[1].2, b"mp3-two");
    assert_eq!(calls[0].3, 0o600);
    // Temp files are gone after playback.
    assert!(
        calls
            .iter()
            .all(|c| !Path::new(c.1.last().unwrap()).exists())
    );
}

#[test]
fn provider_failures_never_play() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    for (status, mime, body) in [
        (401, "application/json", b"{}".to_vec()),
        (200, "text/html", b"<html>".to_vec()),
        (200, "audio/mpeg", Vec::new()),
    ] {
        let (url, _) = serve(vec![(status, mime, body)]);
        let player = FakePlayer::default();
        let error = rt
            .block_on(speech::speak(&deepgram(&url), &player, "Hello.", 1.0))
            .unwrap_err();
        assert!(!error.contains("fixture-token"), "{error}");
        assert!(player.calls.lock().unwrap().is_empty(), "{status} {mime}");
    }
}

#[test]
fn system_voice_reads_text_from_a_private_file_at_the_chosen_rate() {
    let player = FakePlayer::default();
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(speech::speak(
        &Engine::System,
        &player,
        "Tests passed.",
        2.0,
    ))
    .unwrap();
    let calls = player.calls.lock().unwrap();
    assert_eq!(calls[0].0, PathBuf::from("/usr/bin/say"));
    assert_eq!(calls[0].1[..2], ["-r".to_string(), "350".into()]);
    assert_eq!(calls[0].2, b"Tests passed.");
    assert_eq!(calls[0].3, 0o600);
}

#[test]
fn engine_choice_respects_offline_all_and_missing_keys() {
    let settings = Settings::default();
    assert_eq!(settings.voice, Voice::Deepgram);
    let key = || Ok::<_, String>(fotw_secrets::SecretString::new("k"));
    assert!(matches!(
        speech::engine(&settings, false, key),
        Engine::Deepgram { .. }
    ));
    // Offline-all never resolves a key.
    assert!(matches!(
        speech::engine(&settings, true, || -> Result<_, String> {
            panic!("key resolved under Offline-all")
        }),
        Engine::System
    ));
    assert!(matches!(
        speech::engine(&settings, false, || Err("No Deepgram key".to_string())),
        Engine::System
    ));
    let system = Settings {
        voice: Voice::System,
        ..Default::default()
    };
    assert!(matches!(
        speech::engine(&system, false, || -> Result<_, String> {
            panic!("key resolved for the system voice")
        }),
        Engine::System
    ));
}

#[test]
fn voice_settings_command_round_trips_and_rejects_bad_values() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("agent-voice.json");
    assert!(
        speech::command(&["status".into()], &path)
            .unwrap()
            .contains("deepgram")
    );
    speech::command(&["system".into()], &path).unwrap();
    assert_eq!(Settings::load(&path).unwrap().voice, Voice::System);
    speech::command(&["deepgram".into(), "aura-2-thalia-en".into()], &path).unwrap();
    let saved = Settings::load(&path).unwrap();
    assert_eq!(saved.voice, Voice::Deepgram);
    assert_eq!(saved.model, "aura-2-thalia-en");
    speech::command(&["speed".into(), "1.25".into()], &path).unwrap();
    assert_eq!(Settings::load(&path).unwrap().speed, 1.25);
    assert!(speech::command(&["speed".into(), "3".into()], &path).is_err());
    assert!(speech::command(&["deepgram".into(), "bad model!".into()], &path).is_err());
    std::fs::write(&path, b"{not json").unwrap();
    assert!(Settings::load(&path).is_err());
}

#[test]
fn the_mcp_voice_rereads_offline_all_and_settings_but_caches_the_key() {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    let (url, seen) = serve(vec![
        (200, "audio/mpeg", b"one".to_vec()),
        (200, "audio/mpeg", b"two".to_vec()),
    ]);
    let offline = Arc::new(AtomicBool::new(false));
    let keys = Arc::new(AtomicUsize::new(0));
    let player = FakePlayer::default();
    let (o, k) = (offline.clone(), keys.clone());
    let voice = speech::Voicebox::new(
        Box::new(move || Ok((Settings::default(), o.load(Ordering::SeqCst)))),
        Box::new(move || {
            k.fetch_add(1, Ordering::SeqCst);
            Ok(fotw_secrets::SecretString::new("fixture-token"))
        }),
        url,
        Box::new(player.clone()),
    );
    voice.speak("First.", None).unwrap();
    offline.store(true, Ordering::SeqCst);
    voice.speak("Private.", None).unwrap();
    offline.store(false, Ordering::SeqCst);
    voice.speak("Third.", None).unwrap();
    let calls = player.calls.lock().unwrap();
    let programs: Vec<_> = calls.iter().map(|c| c.0.clone()).collect();
    assert_eq!(
        programs,
        [
            PathBuf::from("/usr/bin/afplay"),
            PathBuf::from("/usr/bin/say"),
            PathBuf::from("/usr/bin/afplay")
        ]
    );
    let requests = seen.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(requests.iter().all(|r| !r.contains("Private.")));
    assert_eq!(
        keys.load(Ordering::SeqCst),
        1,
        "Keychain read more than once"
    );
}

#[cfg(unix)]
#[test]
fn a_stop_during_player_start_still_stops_playback() {
    // Stop before play: the child must never keep running.
    let playback = speech::Playback::default();
    playback.stop();
    let started = std::time::Instant::now();
    let result = playback.play(
        Path::new("/bin/sleep"),
        &["5".into()],
        Path::new("/dev/null"),
    );
    assert!(result.is_err());
    assert!(started.elapsed() < std::time::Duration::from_secs(2));
    // Stop racing a running player.
    let playback = speech::Playback::default();
    let stopper = playback.clone();
    let handle = std::thread::spawn(move || {
        playback.play(
            Path::new("/bin/sleep"),
            &["5".into()],
            Path::new("/dev/null"),
        )
    });
    std::thread::sleep(std::time::Duration::from_millis(200));
    let started = std::time::Instant::now();
    stopper.stop();
    assert!(handle.join().unwrap().is_err());
    assert!(started.elapsed() < std::time::Duration::from_secs(2));
}
