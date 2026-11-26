//! Dictation routed to the active agent with a spoken reply (#114). The
//! menu bar calls [`deliver`] off the main thread and shows each [`Stage`].
use super::{
    client,
    speech::{Player, Settings, Voicebox},
};
use std::path::Path;

#[derive(Debug, PartialEq, Eq)]
pub enum Destination {
    /// Send to the active Murmur agent session and speak its reply.
    Agent,
    /// Murmur's existing delivery (focused field or remote review).
    Insert,
}

/// Agent voice takes local dictation only; remote review keeps its route.
pub fn destination(agent_voice: bool, remote_mode: bool, remote_target: bool) -> Destination {
    if agent_voice && !remote_mode && !remote_target {
        Destination::Agent
    } else {
        Destination::Insert
    }
}

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

pub fn routed(path: &Path) -> bool {
    Settings::load(path).is_ok_and(|s| s.route_dictation)
}

pub fn set_routed(path: &Path, on: bool) -> Result<(), String> {
    let mut settings = Settings::load(path)?;
    settings.route_dictation = on;
    settings.save(path)
}
