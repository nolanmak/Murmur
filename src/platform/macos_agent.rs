//! Native "Agent key" toggle (#114): holding Right Option talks to the
//! active agent session. Control dictation is never affected. Saved with the
//! agent voice settings; `murmur agent key on|off` changes it live.
use crate::agent::{speech::Settings, voice};
use muda::MenuItem;
use std::path::PathBuf;

pub const TOGGLE: &str = "agent_key";

pub struct AgentMenu {
    pub item: MenuItem,
    settings: Result<PathBuf, String>,
    on: bool,
    checked: std::time::Instant,
}

pub fn label(on: bool) -> &'static str {
    if on {
        "Agent key (hold Right Option): On"
    } else {
        "Agent key (hold Right Option): Off"
    }
}

impl AgentMenu {
    pub fn new(settings: Result<PathBuf, String>) -> Self {
        let on = settings.as_ref().is_ok_and(|path| voice::key_enabled(path));
        Self {
            item: MenuItem::with_id(TOGGLE, label(on), settings.is_ok(), None),
            settings,
            on,
            checked: std::time::Instant::now(),
        }
    }

    /// Re-reads the saved setting, so `murmur agent key on|off` applies to
    /// the running app.
    pub fn refresh(&mut self) {
        self.checked = std::time::Instant::now();
        let on = self
            .settings
            .as_ref()
            .is_ok_and(|path| voice::key_enabled(path));
        if on != self.on {
            self.on = on;
            self.item.set_text(label(on));
        }
    }

    /// [`refresh`](Self::refresh) at most once a second; returns whether the
    /// setting changed.
    pub fn poll(&mut self) -> bool {
        if self.checked.elapsed() < std::time::Duration::from_secs(1) {
            return false;
        }
        let was = self.on;
        self.refresh();
        was != self.on
    }

    pub fn on(&self) -> bool {
        self.on
    }

    /// Handles this menu's events; returns the status to show.
    pub fn handle(&mut self, id: &str) -> Option<Result<&'static str, String>> {
        if id != TOGGLE {
            return None;
        }
        let path = match &self.settings {
            Ok(path) => path,
            Err(error) => return Some(Err(error.clone())),
        };
        if let Err(error) = voice::set_key_enabled(path, !self.on) {
            return Some(Err(error));
        }
        self.on = !self.on;
        self.item.set_text(label(self.on));
        Some(Ok(if self.on {
            "Agent key on · hold Right Option to talk to your agent"
        } else {
            "Agent key off · Control dictation unchanged"
        }))
    }

    /// The saved settings path, for callers that read other agent settings.
    pub fn settings(&self) -> Option<&PathBuf> {
        self.settings.as_ref().ok()
    }
}

/// Default settings location.
pub fn settings_path() -> Result<PathBuf, String> {
    Settings::path()
}
