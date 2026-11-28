//! Talking to the active agent with a spoken reply (#114). The menu bar
//! records on the agent key (Option), calls [`deliver`] off the main
//! thread and shows each [`Stage`]. Control dictation never comes here.
use super::{
    client,
    speech::{Player, Settings, Voicebox},
};
use std::path::Path;

#[derive(Debug, PartialEq, Eq)]
pub enum Stage {
    Sending,
    /// The native input box took the turn; the agent is working.
    Waiting,
    Speaking,
    Replied(&'static str),
    /// A new dictation stopped the reply; nothing to report.
    Stopped,
    Failed(String),
}

impl Stage {
    /// Status text. Never includes transcript or reply text.
    pub fn message(&self) -> Option<String> {
        Some(match self {
            Stage::Sending => "◌ Sending to agent…".into(),
            Stage::Waiting => "◌ Agent is answering…".into(),
            Stage::Speaking => "🔊 Speaking agent reply…".into(),
            Stage::Replied(voice) => format!("Agent replied · {voice}"),
            Stage::Stopped => return None,
            Stage::Failed(error) => error.clone(),
        })
    }
}

pub fn deliver(
    dir: &Path,
    text: &str,
    voice: &Voicebox,
    player: &dyn Player,
    mut report: impl FnMut(Stage),
) {
    report(Stage::Sending);
    let reply = client::send_events(dir, None, text, |event| {
        if let client::Event::Accepted = event {
            report(Stage::Waiting);
        }
    });
    let reply = match reply {
        Ok(reply) => reply,
        // A newer agent turn took over; it reports for itself.
        Err(error) if error.starts_with("Superseded") => return report(Stage::Stopped),
        Err(error) => return report(Stage::Failed(error)),
    };
    let reply: String = reply.trim().chars().take(super::speech::MAX_TEXT).collect();
    if reply.is_empty() {
        return report(Stage::Replied("no spoken text"));
    }
    report(Stage::Speaking);
    match voice.speak_with(&reply, None, player) {
        Ok(label) => report(Stage::Replied(label)),
        Err(error) if error == "Speech stopped" => report(Stage::Stopped),
        Err(error) => report(Stage::Failed(error)),
    }
}

/// `murmur agent key on|off|status`. The running menu bar picks the change
/// up within a second.
pub fn key_command(args: &[String], path: &Path) -> Result<String, String> {
    match args.iter().map(String::as_str).collect::<Vec<_>>()[..] {
        ["on"] => set_key_enabled(path, true)?,
        ["off"] => set_key_enabled(path, false)?,
        ["status"] | [] => {}
        _ => return Err(KEY_USAGE.into()),
    }
    Ok(if key_enabled(path) {
        "Agent key: on · hold Option to talk to the active Murmur agent session. Control stays dictation.".into()
    } else {
        "Agent key: off · Control stays dictation.".into()
    })
}
pub const KEY_USAGE: &str = "Usage: murmur agent key on | off | status";

pub fn key_enabled(path: &Path) -> bool {
    Settings::load(path).is_ok_and(|s| s.agent_key)
}

pub fn set_key_enabled(path: &Path, on: bool) -> Result<(), String> {
    let mut settings = Settings::load(path)?;
    settings.agent_key = on;
    settings.save(path)
}
