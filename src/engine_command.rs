//! Spoken engine switching (#96).
//!
//! A command is the *whole* utterance: the wake word "Murmur" followed by one
//! known phrase, ignoring case and punctuation. Anything else, including the
//! same phrase inside a longer sentence, is ordinary dictation. A recognized
//! command is never inserted, and a switch is refused rather than landing on an
//! engine that cannot transcribe.
use crate::speech::Provider;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    Local,
    Deepgram,
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
        Provider::LocalWhisper => "Local Whisper",
        Provider::DeepgramThenLocal => "Deepgram with local backup",
    }
}

fn provider(target: Target) -> Provider {
    match target {
        Target::Local => Provider::LocalWhisper,
        Target::Deepgram => Provider::Deepgram,
        Target::Backup => Provider::DeepgramThenLocal,
    }
}

/// Decide what a command does, given the saved engine, the engine before the
/// last voice switch, the Offline-all policy and a fresh local readiness probe.
pub fn decide(
    command: Command,
    current: Option<&Provider>,
    previous: Option<&Provider>,
    offline_all: bool,
    local: &Result<(), String>,
) -> Decision {
    let current_label = current.map(label).unwrap_or("the current setting");
    let target = match command {
        Command::Which => {
            let suffix = if offline_all {
                " · Offline-all is on"
            } else {
                ""
            };
            return Decision::Report(format!("Using {current_label}{suffix}"));
        }
        Command::Use(target) => provider(target),
        Command::Undo => match previous {
            Some(previous) => previous.clone(),
            None => return Decision::Report("Nothing to undo".into()),
        },
    };
    if current == Some(&target) {
        return Decision::Report(format!("Already using {}", label(&target)));
    }
    let cloud = !matches!(target, Provider::LocalWhisper);
    if cloud && offline_all {
        return Decision::Refuse(format!(
            "Offline-all is on — switch to {} from the menu; staying on {current_label}",
            label(&target)
        ));
    }
    let needs_local = !matches!(target, Provider::Deepgram);
    if needs_local && let Err(reason) = local {
        return Decision::Refuse(format!("{reason} — staying on {current_label}"));
    }
    Decision::Switch {
        message: format!("Switched to {} · next dictation", label(&target)),
        to: target,
    }
}
