//! Spoken engine switching (#96).
//!
//! A command is the *whole* utterance: the wake word "Murmur" followed by one
//! known phrase, ignoring case and punctuation. Anything else, including the
//! same phrase inside a longer sentence, is ordinary dictation. A recognized
//! command is never inserted, and a switch is refused rather than landing on an
//! engine that cannot transcribe.
//!
//! Two local engines exist (#101). "Local" and "backup" mean the default local
//! engine (Parakeet on a new install, Whisper for a settings file from before
//! Parakeet); "Parakeet" and "Whisper" name one, and choosing either makes it
//! the default.
use crate::speech::{LocalEngine, Provider};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    /// The default local engine.
    Local,
    /// Local Parakeet by name.
    Parakeet,
    /// Local Whisper by name.
    Whisper,
    Deepgram,
    /// Deepgram with the default local engine as backup.
    Backup,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    Use(Target),
    Undo,
    Which,
}

impl Command {
    /// Whether applying this command may depend on the local engine, so the
    /// worker probes its readiness before the result reaches the UI.
    pub fn may_need_local(self) -> bool {
        !matches!(self, Command::Which | Command::Use(Target::Deepgram))
    }
    /// Whether applying this command may land on cloud-only Deepgram, so the
    /// worker checks that a key loads. Backup falls back to local without one.
    pub fn may_need_cloud(self) -> bool {
        matches!(self, Command::Undo | Command::Use(Target::Deepgram))
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Decision {
    /// Save `to` for the next utterance and show `message`.
    Switch { to: Provider, message: String },
    /// Keep the current engine and explain why.
    Refuse(String),
    /// Nothing changes; show `message`.
    Report(String),
}

// Speech engines spell an unfamiliar name in a few ways.
const WAKE_WORDS: &[&str] = &["murmur", "murmer", "murmor", "mermer"];

/// Recognize a command only when it is the entire utterance.
pub fn parse(raw: &str) -> Option<Command> {
    let words: Vec<String> = raw
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_lowercase)
        .collect();
    let (wake, rest) = words.split_first()?;
    if !WAKE_WORDS.contains(&wake.as_str()) {
        return None;
    }
    let phrase = rest.join(" ").replace("deep gram", "deepgram");
    Some(match phrase.as_str() {
        "use local" | "switch to local" | "go local" => Command::Use(Target::Local),
        "use parakeet" | "switch to parakeet" | "use local parakeet" => {
            Command::Use(Target::Parakeet)
        }
        "use whisper" | "switch to whisper" | "use local whisper" => Command::Use(Target::Whisper),
        "use deepgram" | "switch to deepgram" | "use cloud" | "switch to cloud" => {
            Command::Use(Target::Deepgram)
        }
        "use backup"
        | "switch to backup"
        | "use deepgram with backup"
        | "use cloud with backup"
        | "use deepgram with local backup" => Command::Use(Target::Backup),
        "undo" | "switch back" | "go back" => Command::Undo,
        "which engine" | "what engine" | "which model" | "what model" => Command::Which,
        _ => return None,
    })
}

pub fn label(provider: &Provider) -> &'static str {
    match provider {
        Provider::Deepgram => "Deepgram",
        Provider::LocalParakeet => "Local Parakeet",
        Provider::LocalWhisper => "Local Whisper",
        Provider::DeepgramThenParakeet => "Deepgram with Parakeet backup",
        Provider::DeepgramThenWhisper => "Deepgram with Whisper backup",
    }
}

fn provider(target: Target, default_local: LocalEngine) -> Provider {
    match target {
        Target::Local => Provider::local(default_local),
        Target::Parakeet => Provider::LocalParakeet,
        Target::Whisper => Provider::LocalWhisper,
        Target::Deepgram => Provider::Deepgram,
        Target::Backup => Provider::backup(default_local),
    }
}

/// The engine a command would switch to, so the worker can probe the local
/// engine that choice needs before the result reaches the UI.
pub fn target(
    command: Command,
    previous: Option<&Provider>,
    default_local: LocalEngine,
) -> Option<Provider> {
    match command {
        Command::Which => None,
        Command::Use(target) => Some(provider(target, default_local)),
        Command::Undo => previous.cloned(),
    }
}

/// Decide what a command does, given the saved engine, the engine before the
/// last voice switch, the default local engine, the Offline-all policy and
/// fresh probes of the target's local engine and the cloud key.
pub fn decide(
    command: Command,
    current: Option<&Provider>,
    previous: Option<&Provider>,
    default_local: LocalEngine,
    offline_all: bool,
    local: &Result<(), String>,
    cloud_key: &Result<(), String>,
) -> Decision {
    let current_label = current.map(label).unwrap_or("the current setting");
    let Some(target) = target(command, previous, default_local) else {
        return Decision::Report(match command {
            // Offline-all overrides the saved engine for every dictation.
            Command::Which if offline_all => format!(
                "Using {} · Offline-all is on",
                current
                    .and_then(Provider::local_engine)
                    .unwrap_or(default_local)
                    .label()
            ),
            Command::Which => format!("Using {current_label}"),
            _ => "Nothing to undo".into(),
        });
    };
    if current == Some(&target) {
        return Decision::Report(format!("Already using {}", label(&target)));
    }
    if target.uses_cloud() && offline_all {
        return Decision::Refuse(format!(
            "Offline-all is on — switch to {} from the menu; staying on {current_label}",
            label(&target)
        ));
    }
    if matches!(target, Provider::Deepgram)
        && let Err(reason) = cloud_key
    {
        return Decision::Refuse(format!(
            "{reason} — add a Deepgram key first; staying on {current_label}"
        ));
    }
    if target.local_engine().is_some()
        && let Err(reason) = local
    {
        return Decision::Refuse(format!("{reason} — staying on {current_label}"));
    }
    Decision::Switch {
        message: format!("Switched to {} · next dictation", label(&target)),
        to: target,
    }
}

/// Probe the local engine a spoken switch would land on, in the profile's
/// language, so the switch is refused rather than saved when that engine
/// could not take the next dictation.
pub async fn probe_local(
    settings: &crate::speech::Settings,
    engine: LocalEngine,
    language: &str,
) -> Result<(), String> {
    // The saved language, as the next dictation will use it: Parakeet is
    // English only, and a Whisper model may lack the language.
    settings
        .local_transcriber(engine)?
        .prepare(language, std::time::Duration::from_secs(2))
        .await
        .map(|_| ())
}
