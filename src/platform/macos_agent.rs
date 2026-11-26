//! Native "Agent voice" toggle (#114). The route is saved with the agent
//! voice settings so it survives restarts.
use crate::agent::{speech::Settings, voice};
use muda::MenuItem;
use std::path::PathBuf;

pub const TOGGLE: &str = "agent_voice";

pub struct AgentMenu {
    pub item: MenuItem,
    settings: Result<PathBuf, String>,
    on: bool,
}

pub fn label(on: bool) -> &'static str {
    if on {
        "Agent voice: On (dictation goes to your agent)"
    } else {
        "Agent voice: Off"
    }
}

impl AgentMenu {
    pub fn new(settings: Result<PathBuf, String>) -> Self {
        let on = settings.as_ref().is_ok_and(|path| voice::routed(path));
        Self {
            item: MenuItem::with_id(TOGGLE, label(on), settings.is_ok(), None),
            settings,
            on,
        }
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
        if let Err(error) = voice::set_routed(path, !self.on) {
            return Some(Err(error));
        }
        self.on = !self.on;
        self.item.set_text(label(self.on));
        Some(Ok(if self.on {
            "Agent voice on · dictation goes to your Murmur agent session"
        } else {
            "Agent voice off · dictation inserts text again"
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
