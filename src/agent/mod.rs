//! Agent voice: Agentflow's features on Murmur's speech stack (#112–#114).
pub mod cli;
#[cfg(unix)]
pub mod client;
#[cfg(unix)]
pub mod daemon;
pub mod history;
pub mod mcp;
pub mod reply;
pub mod speech;
pub mod terminal;
#[cfg(unix)]
pub mod voice;

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
