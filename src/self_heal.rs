//! Automatic recovery from microphone ownership Murmur cannot clear itself.
//!
//! An unconfirmed Stop or failed cleanup keeps the startup lease forever by
//! design (#74/#75): Murmur cannot prove the native objects are gone, so it
//! refuses to open another input beside them. A native call blocked inside
//! coreaudiod holds the lease the same way. Neither can be released in-process
//! — the capture objects are already dropped, or their owner thread is stuck in
//! the OS — but process exit releases every Core Audio resource the process
//! owns. So when ownership is stuck and nothing is being dictated, reviewed or
//! installed, Murmur relaunches itself.
//!
//! Relaunching never resumes the old dictation and never inserts its text: it
//! only happens while the shell is idle. A persisted log bounds it to
//! [`Policy::limit`] automatic relaunches per [`Policy::window`]; after that,
//! and whenever the log cannot be written, the user gets a click-to-restart
//! pill instead of a loop.
use crate::capture::MicrophoneState;
use crate::core::Phase;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Shown for [`Policy::notice`] before an automatic relaunch.
pub const RESTARTING: &str = "Resetting microphone — restarting Murmur…";
/// Shown after relaunching, so the restart is not a mystery.
pub const RESTARTED: &str = "Microphone reset — Murmur restarted";
/// A native operation that outlived its grace period.
pub const STUCK: &str = "Mic stuck — click to restart Murmur";

#[derive(Clone, Copy, Debug)]
pub struct Policy {
    /// How long the restart notice shows before the relaunch.
    pub notice: Duration,
    /// How long a pending native operation may hold the lease while idle.
    pub grace: Duration,
    /// Automatic relaunches allowed per `window`.
    pub limit: usize,
    pub window: Duration,
}
impl Default for Policy {
    fn default() -> Self {
        Self {
            notice: Duration::from_millis(1500),
            // Every bounded handoff is four seconds; a still-pending native
            // operation this long after the shell went idle is blocked in the OS.
            grace: Duration::from_secs(30),
            limit: 2,
            window: Duration::from_secs(10 * 60),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Wait,
    /// Show [`RESTARTING`]; a relaunch follows unless work begins.
    Announce,
    /// Already recorded in the log: exit and relaunch now.
    Relaunch,
    /// The loop guard refused: offer the manual restart only.
    Manual,
}

/// One process's view of a stuck episode; time is monotonic since launch.
#[derive(Debug)]
pub struct SelfHeal {
    policy: Policy,
    since: Option<Duration>,
    announced: Option<Duration>,
    stuck: bool,
    manual: bool,
    relaunched: bool,
}
impl SelfHeal {
    pub fn new(policy: Policy) -> Self {
        Self {
            policy,
            since: None,
            announced: None,
            stuck: false,
            manual: false,
            relaunched: false,
        }
    }
    /// `busy`: a dictation, review or install that a relaunch would lose.
    /// `wall`: Unix seconds, for the persisted loop guard.
    pub fn observe(
        &mut self,
        now: Duration,
        state: MicrophoneState,
        busy: bool,
        log: &mut RestartLog,
        wall: u64,
    ) -> Action {
        if state == MicrophoneState::Idle {
            *self = Self::new(self.policy);
            return Action::Wait;
        }
        if busy {
            // Never during dictation. A notice already shown is withdrawn;
            // once idle again, a fresh one precedes any relaunch.
            self.since = None;
            self.announced = None;
            self.stuck = false;
            return Action::Wait;
        }
        let since = *self.since.get_or_insert(now);
        let threshold = match state {
            // These never clear on their own: the lease outlived its objects.
            MicrophoneState::Unconfirmed | MicrophoneState::CleanupFailed => Duration::ZERO,
            _ => self.policy.grace,
        };
        if now.saturating_sub(since) < threshold {
            return Action::Wait;
        }
        self.stuck = true;
        if self.manual || self.relaunched {
            return Action::Wait;
        }
        match self.announced {
            None if log.allows(wall, &self.policy) => {
                self.announced = Some(now);
                Action::Announce
            }
            Some(at) if now.saturating_sub(at) >= self.policy.notice => {
                if log.allows(wall, &self.policy) && log.record(wall).is_ok() {
                    self.relaunched = true;
                    Action::Relaunch
                } else {
                    self.give_up()
                }
            }
            Some(_) => Action::Wait,
            None => self.give_up(),
        }
    }
    /// Stop trying automatically (also used when no app bundle can relaunch).
    pub fn give_up(&mut self) -> Action {
        self.manual = true;
        self.relaunched = false;
        self.announced = None;
        Action::Manual
    }
    /// Ownership is stuck: the pill becomes clickable to restart Murmur.
    pub fn stuck(&self) -> bool {
        self.stuck
    }
    /// The persistent pill/status text for this state.
    pub fn notice(&self, state: MicrophoneState, phase: Phase) -> Option<&'static str> {
        if self.stuck && self.announced.is_some() {
            return Some(RESTARTING);
        }
        let notice = crate::indicator::microphone_notice(state, phase);
        match state {
            MicrophoneState::Unconfirmed | MicrophoneState::CleanupFailed => notice,
            _ if self.stuck => Some(STUCK),
            _ => notice,
        }
    }
}

/// Unix-second timestamps of automatic relaunches, one per line.
#[derive(Debug)]
pub struct RestartLog {
    path: PathBuf,
    entries: Vec<u64>,
}
impl RestartLog {
    /// `~/Library/Application Support/Murmur/microphone-restarts`.
    pub fn default_path() -> Result<PathBuf, String> {
        Ok(
            PathBuf::from(std::env::var_os("HOME").ok_or("Cannot locate home directory")?)
                .join("Library/Application Support/Murmur/microphone-restarts"),
        )
    }
    /// A missing or unreadable log is empty; unparsable lines are ignored.
    pub fn load(path: PathBuf) -> Self {
        let entries = std::fs::read_to_string(&path)
            .map(|text| text.lines().filter_map(|l| l.trim().parse().ok()).collect())
            .unwrap_or_default();
        Self { path, entries }
    }
    pub fn path(&self) -> &Path {
        &self.path
    }
    pub fn latest(&self) -> Option<u64> {
        self.entries.iter().copied().max()
    }
    /// Entries near `wall` count, including ones slightly in the future
    /// (a clock moved back cannot refill the budget).
    fn recent(&self, wall: u64, window: Duration) -> usize {
        let window = window.as_secs();
        self.entries
            .iter()
            .filter(|&&at| at.abs_diff(wall) < window)
            .count()
    }
    fn allows(&self, wall: u64, policy: &Policy) -> bool {
        self.recent(wall, policy.window) < policy.limit
    }
    /// Persist before relaunching; a failed write must stop the relaunch.
    fn record(&mut self, wall: u64) -> Result<(), String> {
        let keep = Policy::default().window.as_secs() * 6;
        let mut entries: Vec<u64> = self
            .entries
            .iter()
            .copied()
            .filter(|at| at.abs_diff(wall) < keep)
            .collect();
        entries.push(wall);
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let text: String = entries.iter().map(|at| format!("{at}\n")).collect();
        let staging = self.path.with_extension("tmp");
        std::fs::write(&staging, text).map_err(|e| e.to_string())?;
        std::fs::rename(&staging, &self.path).map_err(|e| e.to_string())?;
        self.entries = entries;
        Ok(())
    }
    /// This launch followed an automatic relaunch.
    pub fn restarted_recently(&self, wall: u64) -> bool {
        self.latest().is_some_and(|at| wall >= at && wall - at < 60)
    }
}
