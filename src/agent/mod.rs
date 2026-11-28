//! Agentflow's features on Murmur's speech stack: spoken replies
//! (`murmur speak`), the `murmur mcp` tools agents call, and the note that
//! asks an agent to answer a Control+Option dictation aloud.
pub mod cli;
pub mod history;
pub mod mcp;
pub mod speech;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Agent {
    Codex,
    Claude,
}
impl Agent {
    pub fn name(self) -> &'static str {
        match self {
            Agent::Codex => "codex",
            Agent::Claude => "claude",
        }
    }
}

/// Added to a Control+Option dictation so the agent it is typed into
/// answers aloud with `murmur mcp`'s speak tool. One line: multi-line text
/// would be held for review instead of pasted.
pub const SPEAK_NOTE: &str = "(Answer aloud with the speak tool.)";

pub fn speak_note(text: &str) -> String {
    let text = text.trim();
    if text.is_empty() {
        return String::new();
    }
    if text.ends_with(SPEAK_NOTE) {
        return text.to_string();
    }
    format!("{text} {SPEAK_NOTE}")
}
