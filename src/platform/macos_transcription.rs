//! Native Transcription menu (#101): one checked engine choice, the explicit
//! Parakeet model install, and the local models' licenses.
use crate::speech::Provider;
use muda::{CheckMenuItem, MenuItem, PredefinedMenuItem, Submenu};

pub enum Action {
    Choose(Provider),
    InstallParakeet,
}

pub struct TranscriptionMenu {
    pub menu: Submenu,
    items: Vec<(Provider, CheckMenuItem)>,
    install: MenuItem,
}

const INSTALL: &str = "transcription:install-parakeet";

/// The local models' licenses, shown in the menu as the attribution CC-BY-4.0
/// requires. NOTICE in the bundle has the full text.
pub const LICENSES: [&str; 5] = [
    "Parakeet Unified 0.6B (English) · NVIDIA · CC-BY-4.0",
    "Silero VAD · Silero Team · MIT",
    "FluidAudio runtime · Apache-2.0",
    "Whisper base.en · OpenAI · MIT",
    "Full notices: Murmur.app/Contents/Resources/NOTICE",
];

impl TranscriptionMenu {
    pub fn new() -> Result<Self, String> {
        let menu = Submenu::with_id("transcription_menu", "Transcription", true);
        let mut items = Vec::new();
        for provider in Provider::ALL {
            let item = CheckMenuItem::with_id(
                format!("transcription:{}", id(&provider)),
                crate::engine_command::label(&provider),
                true,
                false,
                None,
            );
            menu.append(&item).map_err(|e| e.to_string())?;
            items.push((provider, item));
        }
        menu.append(&PredefinedMenuItem::separator())
            .map_err(|e| e.to_string())?;
        let install = MenuItem::with_id(INSTALL, "Install Parakeet model…", true, None);
        menu.append(&install).map_err(|e| e.to_string())?;
        let licenses = Submenu::new("Local model licenses", true);
        for line in LICENSES {
            licenses
                .append(&MenuItem::new(line, false, None))
                .map_err(|e| e.to_string())?;
        }
        menu.append(&licenses).map_err(|e| e.to_string())?;
        Ok(Self {
            menu,
            items,
            install,
        })
    }

    /// Show `title` and check the saved choice (none when settings are invalid).
    pub fn refresh(&self, selected: Option<&Provider>, title: &str) {
        if self.menu.text() != title {
            self.menu.set_text(title);
        }
        for (provider, item) in &self.items {
            let checked = selected == Some(provider);
            if item.is_checked() != checked {
                item.set_checked(checked);
            }
        }
    }

    /// Choices and installs wait for an idle dictation.
    pub fn set_enabled(&self, enabled: bool) {
        for (_, item) in &self.items {
            item.set_enabled(enabled);
        }
        self.install.set_enabled(enabled);
    }

    pub fn set_install_text(&self, text: &str) {
        self.install.set_text(text);
    }

    pub fn handle(&self, id: &str) -> Option<Action> {
        if id == INSTALL {
            return Some(Action::InstallParakeet);
        }
        let choice = id.strip_prefix("transcription:")?;
        Provider::ALL
            .into_iter()
            .find(|provider| self::id(provider) == choice)
            .map(Action::Choose)
    }
}

fn id(provider: &Provider) -> &'static str {
    match provider {
        Provider::Deepgram => "deepgram",
        Provider::LocalParakeet => "local-parakeet",
        Provider::LocalWhisper => "local-whisper",
        Provider::LocalStreaming => "local-streaming",
        Provider::DeepgramThenParakeet => "deepgram-then-parakeet",
        Provider::DeepgramThenWhisper => "deepgram-then-whisper",
    }
}
