//! Native cleanup/privacy toggles. Credentials and provider calls stay off the UI thread.
use crate::cleanup_config::{NetworkPolicy, Settings};
use muda::{CheckMenuItem, MenuItem, Submenu};
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};
pub struct CleanupMenu {
    pub menu: Submenu,
    enabled: CheckMenuItem,
    offline: CheckMenuItem,
    destination: MenuItem,
    settings: Result<PathBuf, String>,
    policy: Result<PathBuf, String>,
    snapshot: Result<Settings, String>,
    network: Result<NetworkPolicy, String>,
    refreshed: Instant,
}
impl CleanupMenu {
    /// `offline` names the local engine Offline-all runs instead.
    pub fn speech_label<'a>(&self, configured: &'a str, offline: &'a str) -> &'a str {
        if self.network_error() {
            "Transcription: blocked by privacy settings"
        } else if self.offline_all() {
            offline
        } else {
            configured
        }
    }
    pub fn new(
        settings: Result<PathBuf, String>,
        policy: Result<PathBuf, String>,
    ) -> Result<Self, String> {
        let menu = Submenu::new("Text cleanup: Off", true);
        let enabled = CheckMenuItem::with_id(
            "cleanup:toggle",
            "Send utterance text to configured cleanup",
            false,
            false,
            None,
        );
        let offline = CheckMenuItem::with_id(
            "cleanup:offline",
            "Offline-all: local speech, no cleanup",
            true,
            false,
            None,
        );
        let destination = MenuItem::new("Provider not configured", false, None);
        menu.append(&enabled).map_err(|e| e.to_string())?;
        menu.append(&offline).map_err(|e| e.to_string())?;
        menu.append(&destination).map_err(|e| e.to_string())?;
        let mut result = Self {
            menu,
            enabled,
            offline,
            destination,
            settings,
            policy,
            snapshot: Ok(Settings::default()),
            network: Ok(NetworkPolicy::default()),
            refreshed: Instant::now(),
        };
        result.refresh();
        Ok(result)
    }
    pub fn network_error(&self) -> bool {
        self.network.is_err()
    }
    pub fn offline_all(&self) -> bool {
        self.network.as_ref().is_ok_and(|p| p.offline_all)
    }
    pub fn refresh_if_due(&mut self) -> bool {
        if self.refreshed.elapsed() < Duration::from_secs(1) {
            return false;
        }
        self.refresh();
        true
    }
    pub fn refresh(&mut self) {
        self.snapshot = self
            .settings
            .as_ref()
            .map_err(Clone::clone)
            .and_then(|p| Settings::load(p));
        self.network = self
            .policy
            .as_ref()
            .map_err(Clone::clone)
            .and_then(|p| NetworkPolicy::load(p));
        let enabled = self.snapshot.as_ref().is_ok_and(|s| s.enabled);
        let configured = self.snapshot.as_ref().is_ok_and(|s| !s.endpoint.is_empty());
        self.enabled.set_checked(enabled);
        self.enabled
            .set_enabled(configured && !self.offline_all() && !self.network_error());
        self.offline.set_checked(self.offline_all());
        self.menu.set_text(if self.network_error() {
            "Text cleanup: privacy settings need attention"
        } else if self.offline_all() {
            "Text cleanup: paused by Offline-all"
        } else if self.snapshot.is_err() {
            "Text cleanup: settings need attention"
        } else if enabled {
            "Text cleanup: On"
        } else {
            "Text cleanup: Off"
        });
        let target = self
            .snapshot
            .as_ref()
            .ok()
            .filter(|s| !s.endpoint.is_empty())
            .and_then(|s| {
                reqwest::Url::parse(&s.endpoint)
                    .ok()
                    .map(|url| format!("{} · {}", s.model, url.origin().ascii_serialization()))
            })
            .unwrap_or_else(|| "Provider not configured — see Text Cleanup setup".into());
        self.destination.set_text(if target.chars().count() > 100 {
            target.chars().take(97).collect::<String>() + "…"
        } else {
            target
        });
        self.refreshed = Instant::now();
    }
    pub fn handles(id: &str) -> bool {
        matches!(id, "cleanup:toggle" | "cleanup:offline")
    }
    pub fn handle(&mut self, id: &str) -> Option<Result<String, String>> {
        if !Self::handles(id) {
            return None;
        }
        let result = (|| {
            let policy_file = self.policy.as_ref().map_err(Clone::clone)?;
            if id == "cleanup:offline" {
                let current = NetworkPolicy::load(policy_file);
                if current.is_ok() && current != self.network {
                    return Err("Privacy settings changed; review the menu again".into());
                }
                let offline_all = current.map_or(true, |p| !p.offline_all);
                NetworkPolicy {
                    version: 1,
                    offline_all,
                }
                .save(policy_file)?;
                return Ok(if offline_all {
                    "Offline-all enabled · local speech, no cleanup requests".into()
                } else {
                    "Configured speech restored · cleanup stays separately opt-in".into()
                });
            }
            if NetworkPolicy::load(policy_file)?.offline_all {
                return Err("Offline-all blocks cleanup requests".into());
            }
            let file = self.settings.as_ref().map_err(Clone::clone)?;
            let reviewed = self.snapshot.as_ref().map_err(Clone::clone)?;
            Settings::set_enabled(file, reviewed, !reviewed.enabled)?;
            Ok(if reviewed.enabled {
                "Cleanup off · raw transcription".into()
            } else {
                "Cleanup on · only utterance text sent to the displayed provider".into()
            })
        })();
        self.refresh();
        Some(result)
    }
}
