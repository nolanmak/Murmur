//! Spoken replies (#112): Deepgram Aura through Murmur's existing Deepgram
//! credential, or the offline macOS voice. Text is never logged.
use fotw_secrets::SecretString;
use serde::{Deserialize, Serialize};
use std::{
    io::Write,
    path::{Path, PathBuf},
    time::Duration,
};

pub const MAX_TEXT: usize = 12000;
/// Deepgram Aura rejects longer request text.
pub const DEEPGRAM_CHUNK: usize = 1900;
pub const SPEEDS: [f32; 6] = [0.75, 1.0, 1.25, 1.5, 1.75, 2.0];
pub const DEEPGRAM: &str = "https://api.deepgram.com";
const MAX_AUDIO: usize = 24 * 1024 * 1024;
/// `say`'s default rate in words per minute.
const SAY_RATE: f32 = 175.0;

/// Plays a prepared file with a fixed system program. Tests use a fake.
pub trait Player {
    fn play(&self, program: &Path, args: &[String], file: &Path) -> Result<(), String>;
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Voice {
    #[default]
    Deepgram,
    System,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub version: u32,
    pub voice: Voice,
    pub model: String,
    pub speed: f32,
    /// #114: send finished dictations to the active agent instead of pasting.
    pub route_dictation: bool,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            version: 1,
            voice: Voice::Deepgram,
            model: "aura-2-helena-en".into(),
            speed: 1.0,
            route_dictation: false,
        }
    }
}
impl Settings {
    pub fn path() -> Result<PathBuf, String> {
        Ok(
            PathBuf::from(std::env::var_os("HOME").ok_or("Cannot locate home directory")?)
                .join("Library/Application Support/Murmur/agent-voice.json"),
        )
    }
    pub fn load(path: &Path) -> Result<Self, String> {
        let bytes = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(_) => return Err("Cannot read agent voice settings".into()),
        };
        if bytes.len() > 16384 {
            return Err("Agent voice settings too large".into());
        }
        let settings: Self =
            serde_json::from_slice(&bytes).map_err(|_| "Invalid agent voice settings")?;
        settings.validate()?;
        Ok(settings)
    }
    pub fn save(&self, path: &Path) -> Result<(), String> {
        self.validate()?;
        let dir = path.parent().ok_or("Invalid settings directory")?;
        std::fs::create_dir_all(dir).map_err(|_| "Cannot create settings directory")?;
        let mut temp =
            tempfile::NamedTempFile::new_in(dir).map_err(|_| "Cannot create settings file")?;
        serde_json::to_writer_pretty(&mut temp, self).map_err(|_| "Cannot write settings")?;
        temp.as_file()
            .sync_all()
            .map_err(|_| "Cannot sync settings")?;
        temp.persist(path).map_err(|_| "Cannot save settings")?;
        Ok(())
    }
    fn validate(&self) -> Result<(), String> {
        speed(self.speed as f64)?;
        valid_model(&self.model)
    }
}

fn valid_model(model: &str) -> Result<(), String> {
    if model.is_empty()
        || model.len() > 64
        || !model
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    {
        return Err("Use a Deepgram Aura model name such as aura-2-helena-en".into());
    }
    Ok(())
}

pub enum Engine {
    Deepgram {
        base: String,
        key: SecretString,
        model: String,
    },
    System,
}
impl Engine {
    pub fn label(&self) -> &'static str {
        match self {
            Engine::Deepgram { .. } => "Deepgram",
            Engine::System => "macOS voice",
        }
    }
}

pub fn validate_text(text: &str) -> Result<&str, String> {
    let text = text.trim();
    if text.is_empty() || text.chars().count() > MAX_TEXT {
        return Err(format!("Speech text must contain 1–{MAX_TEXT} characters"));
    }
    Ok(text)
}

pub fn speed(value: f64) -> Result<f32, String> {
    SPEEDS
        .iter()
        .copied()
        .find(|s| (*s as f64 - value).abs() < 1e-6)
        .ok_or_else(|| "Use a speed of 0.75, 1, 1.25, 1.5, 1.75 or 2".into())
}

/// Splits at sentence ends (then whitespace) so no part exceeds `max` chars.
pub fn chunks(text: &str, max: usize) -> Vec<String> {
    let mut sentences = Vec::new();
    let mut current = String::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        current.push(c);
        if matches!(c, '.' | '!' | '?' | '\n') && chars.peek().is_none_or(|n| n.is_whitespace()) {
            sentences.push(std::mem::take(&mut current));
        }
    }
    sentences.push(current);
    let mut out: Vec<String> = Vec::new();
    let push = |piece: &str, out: &mut Vec<String>| {
        let piece = piece.trim();
        if piece.is_empty() {
            return;
        }
        match out.last_mut() {
            Some(last) if last.chars().count() + 1 + piece.chars().count() <= max => {
                last.push(' ');
                last.push_str(piece);
            }
            _ => out.push(piece.to_string()),
        }
    };
    for sentence in sentences {
        if sentence.trim().chars().count() <= max {
            push(&sentence, &mut out);
            continue;
        }
        // A run-on sentence: split on whitespace, hard-split overlong words.
        let mut part = String::new();
        for word in sentence.split_whitespace() {
            let mut word = word.to_string();
            while word.chars().count() > max {
                let head: String = word.chars().take(max).collect();
                word = word.chars().skip(max).collect();
                if !part.is_empty() {
                    out.push(std::mem::take(&mut part));
                }
                out.push(head);
            }
            if !part.is_empty() && part.chars().count() + 1 + word.chars().count() > max {
                out.push(std::mem::take(&mut part));
            }
            if !part.is_empty() {
                part.push(' ');
            }
            part.push_str(&word);
        }
        if !part.is_empty() {
            out.push(part);
        }
    }
    out
}

/// Picks the voice. Offline-all and the system choice never resolve a key;
/// a missing key falls back to the offline voice.
pub fn engine(
    settings: &Settings,
    offline_all: bool,
    key: impl FnOnce() -> Result<SecretString, String>,
) -> Engine {
    if offline_all || settings.voice == Voice::System {
        return Engine::System;
    }
    match key() {
        Ok(key) => Engine::Deepgram {
            base: DEEPGRAM.into(),
            key,
            model: settings.model.clone(),
        },
        Err(_) => Engine::System,
    }
}

/// Engine from saved settings, Offline-all and Murmur's Deepgram credential.
pub fn configured() -> Result<(Engine, Settings), String> {
    let settings = Settings::load(&Settings::path()?)?;
    let offline =
        crate::cleanup_config::NetworkPolicy::load(&crate::cleanup_config::NetworkPolicy::path()?)?
            .offline_all;
    let engine = engine(&settings, offline, || {
        crate::config::load().map(|credentials| credentials.key)
    });
    Ok((engine, settings))
}

pub async fn speak(
    engine: &Engine,
    player: &dyn Player,
    text: &str,
    speed: f32,
) -> Result<(), String> {
    let text = validate_text(text)?;
    let speed = self::speed(speed as f64)?;
    match engine {
        Engine::System => {
            let file = private_file(text.as_bytes(), "txt")?;
            let rate = ((SAY_RATE * speed).round() as u32).to_string();
            player.play(
                Path::new("/usr/bin/say"),
                &["-r".into(), rate, "-f".into(), path_arg(file.path())],
                file.path(),
            )
        }
        Engine::Deepgram { base, key, model } => {
            valid_model(model)?;
            let client = reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_secs(60))
                .build()
                .map_err(|_| "Cannot start speech client")?;
            for part in chunks(text, DEEPGRAM_CHUNK) {
                let audio = deepgram(&client, base, key, model, &part).await?;
                let file = private_file(&audio, "mp3")?;
                player.play(
                    Path::new("/usr/bin/afplay"),
                    &[
                        "--rate".into(),
                        format!("{speed}"),
                        "--rQuality".into(),
                        "1".into(),
                        path_arg(file.path()),
                    ],
                    file.path(),
                )?;
            }
            Ok(())
        }
    }
}

async fn deepgram(
    client: &reqwest::Client,
    base: &str,
    key: &SecretString,
    model: &str,
    text: &str,
) -> Result<Vec<u8>, String> {
    let url = format!(
        "{}/v1/speak?model={model}&encoding=mp3",
        base.trim_end_matches('/')
    );
    let mut response = client
        .post(url)
        .header(
            reqwest::header::AUTHORIZATION,
            format!("Token {}", key.expose()),
        )
        .json(&serde_json::json!({ "text": text }))
        .send()
        .await
        .map_err(|_| "Deepgram speech is unreachable")?;
    let status = response.status();
    if !status.is_success() {
        return Err(match status.as_u16() {
            401 | 403 => "Deepgram rejected the speech key".into(),
            429 => "Deepgram speech is rate limited".into(),
            code => format!("Deepgram speech failed (HTTP {code})"),
        });
    }
    let mime = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase();
    if !mime.starts_with("audio/") && !mime.starts_with("application/octet-stream") {
        return Err("Deepgram did not return audio".into());
    }
    let mut audio = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| "Deepgram speech was interrupted")?
    {
        audio.extend_from_slice(&chunk);
        if audio.len() > MAX_AUDIO {
            return Err("Speech output exceeded 24 MB".into());
        }
    }
    if audio.is_empty() {
        return Err("Deepgram returned empty audio".into());
    }
    Ok(audio)
}

/// A 0600 temp file, removed when dropped (after playback or on failure).
fn private_file(bytes: &[u8], extension: &str) -> Result<tempfile::NamedTempFile, String> {
    let mut file = tempfile::Builder::new()
        .prefix("murmur-speech-")
        .suffix(&format!(".{extension}"))
        .tempfile()
        .map_err(|_| "Cannot prepare speech audio")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(file.path(), std::fs::Permissions::from_mode(0o600))
            .map_err(|_| "Cannot prepare speech audio")?;
    }
    file.write_all(bytes)
        .and_then(|_| file.flush())
        .map_err(|_| "Cannot prepare speech audio")?;
    Ok(file)
}

fn path_arg(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

/// Runs the player as a child process. `stop` ends current playback; used
/// when a new dictation starts while a reply is still being spoken (#114).
#[derive(Clone, Default)]
pub struct Playback {
    child: std::sync::Arc<std::sync::Mutex<Option<std::process::Child>>>,
    stopped: std::sync::Arc<std::sync::atomic::AtomicBool>,
}
impl Playback {
    pub fn stop(&self) {
        self.stopped
            .store(true, std::sync::atomic::Ordering::SeqCst);
        if let Some(child) = self
            .child
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_mut()
        {
            let _ = child.kill();
        }
    }
    pub fn stopped(&self) -> bool {
        self.stopped.load(std::sync::atomic::Ordering::SeqCst)
    }
}
impl Player for Playback {
    fn play(&self, program: &Path, args: &[String], _file: &Path) -> Result<(), String> {
        if self.stopped() {
            return Err("Speech stopped".into());
        }
        let child = std::process::Command::new(program)
            .args(args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|_| "Cannot start audio playback")?;
        *self.child.lock().unwrap_or_else(|e| e.into_inner()) = Some(child);
        let deadline = std::time::Instant::now() + Duration::from_secs(600);
        loop {
            let mut guard = self.child.lock().unwrap_or_else(|e| e.into_inner());
            let Some(child) = guard.as_mut() else {
                return Err("Speech stopped".into());
            };
            match child.try_wait() {
                Ok(Some(status)) => {
                    *guard = None;
                    drop(guard);
                    if self.stopped() {
                        return Err("Speech stopped".into());
                    }
                    return if status.success() {
                        Ok(())
                    } else {
                        Err("Audio playback failed".into())
                    };
                }
                Ok(None) if std::time::Instant::now() > deadline => {
                    let _ = child.kill();
                    *guard = None;
                    return Err("Audio playback timed out".into());
                }
                Ok(None) => {}
                Err(_) => return Err("Audio playback failed".into()),
            }
            drop(guard);
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

/// `murmur agent voice …`
pub fn command(args: &[String], path: &Path) -> Result<String, String> {
    let mut settings = Settings::load(path)?;
    match args.iter().map(String::as_str).collect::<Vec<_>>()[..] {
        ["status"] | [] => {}
        ["system"] => settings.voice = Voice::System,
        ["deepgram"] => settings.voice = Voice::Deepgram,
        ["deepgram", model] => {
            valid_model(model)?;
            settings.voice = Voice::Deepgram;
            settings.model = model.into();
        }
        ["speed", value] => {
            settings.speed = speed(value.parse::<f64>().map_err(|_| "Speed must be a number")?)?
        }
        _ => return Err(VOICE_USAGE.into()),
    }
    if !matches!(args.first().map(String::as_str), None | Some("status")) {
        settings.save(path)?;
    }
    Ok(format!(
        "Agent voice: {}{}; speed {}. Offline-all and a missing Deepgram key use the macOS voice.",
        match settings.voice {
            Voice::Deepgram => "deepgram",
            Voice::System => "system (offline)",
        },
        if settings.voice == Voice::Deepgram {
            format!(" ({})", settings.model)
        } else {
            String::new()
        },
        settings.speed
    ))
}
pub const VOICE_USAGE: &str = "Usage: murmur agent voice status | system | deepgram [AURA_MODEL] | speed 0.75|1|1.25|1.5|1.75|2";
