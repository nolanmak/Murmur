//! Agent voice: Agentflow's features on Murmur's speech stack (#112–#114).
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
