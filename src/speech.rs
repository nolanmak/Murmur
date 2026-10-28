//! Persisted provider settings, independent of cloud credentials.
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum Provider {
    #[default]
    Deepgram,
    /// Parakeet Unified through the bundled fotw-speechd helper (#101).
    LocalParakeet,
    LocalWhisper,
    /// Deepgram with Local Whisper as the backup. The name predates Parakeet
    /// and stays for settings files written before it.
    #[serde(rename = "deepgram-then-local")]
    DeepgramThenWhisper,
    DeepgramThenParakeet,
    /// Nemotron 3.5 streaming through the bundled fotw-speechd helper (#100),
    /// falling back to the default local engine. Never cloud.
    LocalStreaming,
}
impl Provider {
    /// Every choice, in menu order.
    pub const ALL: [Provider; 6] = [
        Provider::Deepgram,
        Provider::LocalStreaming,
        Provider::LocalParakeet,
        Provider::LocalWhisper,
        Provider::DeepgramThenParakeet,
        Provider::DeepgramThenWhisper,
    ];
    /// The local engine this choice runs, alone or as the backup.
    pub fn local_engine(&self) -> Option<LocalEngine> {
        match self {
            // Streaming's batch fallback is the default local engine.
            Provider::Deepgram | Provider::LocalStreaming => None,
            Provider::LocalParakeet | Provider::DeepgramThenParakeet => Some(LocalEngine::Parakeet),
            Provider::LocalWhisper | Provider::DeepgramThenWhisper => Some(LocalEngine::Whisper),
        }
    }
    /// Local only, no cloud at all.
    pub fn local(engine: LocalEngine) -> Self {
        match engine {
            LocalEngine::Parakeet => Provider::LocalParakeet,
            LocalEngine::Whisper => Provider::LocalWhisper,
        }
    }
    /// Deepgram, falling back to `engine`.
    pub fn backup(engine: LocalEngine) -> Self {
        match engine {
            LocalEngine::Parakeet => Provider::DeepgramThenParakeet,
            LocalEngine::Whisper => Provider::DeepgramThenWhisper,
        }
    }
    pub fn uses_cloud(&self) -> bool {
        !matches!(
            self,
            Provider::LocalParakeet | Provider::LocalWhisper | Provider::LocalStreaming
        )
    }
}

/// Which local engine "local" means.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum LocalEngine {
    Parakeet,
    Whisper,
}
impl LocalEngine {
    /// Settings written before Parakeet existed keep the engine they used.
    fn legacy() -> Self {
        LocalEngine::Whisper
    }
    pub fn label(self) -> &'static str {
        match self {
            LocalEngine::Parakeet => "Local Parakeet",
            LocalEngine::Whisper => "Local Whisper",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    pub provider: Provider,
    pub local_endpoint: String,
    /// The default local engine: what "Murmur, use local" and "use backup"
    /// select, and what Offline-all runs while Deepgram is chosen. Parakeet on
    /// a new install; a file from before Parakeet has no field and keeps
    /// Whisper, so existing users move only when they choose to.
    #[serde(default = "LocalEngine::legacy")]
    pub local_engine: LocalEngine,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            provider: Provider::Deepgram,
            local_endpoint: "http://127.0.0.1:17863".into(),
            local_engine: LocalEngine::Parakeet,
        }
    }
}
impl Settings {
    /// The engine local dictation uses under this choice, or under
    /// Offline-all while a cloud-only choice is saved.
    pub fn effective_local_engine(&self) -> LocalEngine {
        self.provider.local_engine().unwrap_or(self.local_engine)
    }
    /// The local transcriber for `engine`.
    pub fn local_transcriber(
        &self,
        engine: LocalEngine,
    ) -> Result<crate::local_stt::LocalTranscriber, String> {
        Ok(match engine {
            LocalEngine::Whisper => crate::local_stt::LocalTranscriber::Whisper(
                crate::local_stt::LocalEndpoint::new(&self.local_endpoint)?,
            ),
            LocalEngine::Parakeet => crate::local_stt::LocalTranscriber::Parakeet(
                crate::local_parakeet::Endpoint::shared(),
            ),
        })
    }
    /// Record `provider`; a choice that names a local engine also makes it the
    /// default local engine.
    pub fn select(&mut self, provider: Provider) {
        if let Some(engine) = provider.local_engine() {
            self.local_engine = engine;
        }
        self.provider = provider;
    }
    pub fn capture_provider(
        &self,
        credentials: impl FnOnce() -> Result<crate::config::Credentials, String>,
    ) -> Result<crate::capture::CaptureProvider, String> {
        match self.provider {
            Provider::Deepgram => {
                let credentials = credentials()?;
                Ok(crate::capture::CaptureProvider::Deepgram {
                    key: credentials.key.expose().into(),
                    endpoint: fotw_stt::DeepgramEndpoint::production(),
                })
            }
            Provider::DeepgramThenWhisper | Provider::DeepgramThenParakeet => {
                let local = self.local_transcriber(self.effective_local_engine())?;
                match credentials() {
                    Ok(credentials) => Ok(crate::capture::CaptureProvider::DeepgramThenLocal {
                        key: credentials.key.expose().into(),
                        endpoint: fotw_stt::DeepgramEndpoint::production(),
                        local,
                    }),
                    Err(_) => Ok(crate::capture::CaptureProvider::Local(local)),
                }
            }
            Provider::LocalWhisper | Provider::LocalParakeet => {
                Ok(crate::capture::CaptureProvider::Local(
                    self.local_transcriber(self.effective_local_engine())?,
                ))
            }
            Provider::LocalStreaming => self.streaming(),
        }
    }

    fn streaming(&self) -> Result<crate::capture::CaptureProvider, String> {
        Ok(crate::capture::CaptureProvider::Streaming {
            stream: crate::local_streaming::Endpoint::shared(),
            // The batch engine for when streaming isn't ready or fails
            // mid-utterance: the default local engine (Parakeet on a new
            // install).
            fallback: Some(self.local_transcriber(self.effective_local_engine())?),
        })
    }

    pub fn path() -> Result<PathBuf, String> {
        Ok(
            PathBuf::from(std::env::var_os("HOME").ok_or("Cannot locate home directory")?)
                .join("Library/Application Support/Murmur/speech.json"),
        )
    }
    pub fn load(path: &Path) -> Result<Self, String> {
        match std::fs::read(path) {
            Ok(bytes) => {
                let settings: Self = serde_json::from_slice(&bytes)
                    .map_err(|_| "Invalid speech settings; check speech.json")?;
                settings.validate()?;
                Ok(settings)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::unsaved(path)),
            Err(_) => Err("Cannot read speech settings".into()),
        }
    }
    /// Settings before any choice is saved. Parakeet is the local engine on a
    /// new machine, but someone who installed the managed Whisper runtime and
    /// never saved a choice already relies on Whisper (Offline-all, "use
    /// local"), so they keep it until they choose.
    fn unsaved(path: &Path) -> Self {
        let mut settings = Self::default();
        if whisper_runtime_installed(path) {
            settings.local_engine = LocalEngine::Whisper;
        }
        settings
    }
    pub fn validate(&self) -> Result<(), String> {
        crate::local_stt::LocalEndpoint::new(&self.local_endpoint).map(|_| ())
    }
    pub fn save(&self, path: &Path) -> Result<(), String> {
        self.validate()?;
        let parent = path.parent().ok_or("Invalid settings path")?;
        std::fs::create_dir_all(parent).map_err(|_| "Cannot create settings directory")?;
        let mut temp =
            tempfile::NamedTempFile::new_in(parent).map_err(|_| "Cannot create settings file")?;
        serde_json::to_writer(&mut temp, self).map_err(|_| "Cannot write speech settings")?;
        temp.persist(path)
            .map_err(|_| "Cannot save speech settings")?;
        Ok(())
    }
}

/// A recoverable configuration failure must never silently select a cloud provider.
#[derive(Clone, Debug)]
pub struct Selection(Result<Settings, String>);
impl Selection {
    /// Offline-all selects the configured local endpoint without cloud credentials.
    pub fn capture_provider_with_privacy(
        &self,
        offline_all: bool,
        credentials: impl FnOnce() -> Result<crate::config::Credentials, String>,
    ) -> Result<crate::capture::CaptureProvider, String> {
        if offline_all {
            let settings = self.0.as_ref().map_err(Clone::clone)?;
            // Streaming is local already; everything else runs the local engine.
            if settings.provider == Provider::LocalStreaming {
                return settings.streaming();
            }
            return Ok(crate::capture::CaptureProvider::Local(
                settings.local_transcriber(settings.effective_local_engine())?,
            ));
        }
        self.capture_provider(credentials)
    }
    pub fn load(path: Result<PathBuf, String>) -> Self {
        Self(path.and_then(|path| Settings::load(&path)))
    }
    pub fn error(&self) -> Option<&str> {
        self.0.as_ref().err().map(String::as_str)
    }
    pub fn provider(&self) -> Option<&Provider> {
        self.0.as_ref().ok().map(|s| &s.provider)
    }
    pub fn local_endpoint(&self) -> Option<&str> {
        self.0.as_ref().ok().map(|s| s.local_endpoint.as_str())
    }
    pub fn settings(&self) -> Option<&Settings> {
        self.0.as_ref().ok()
    }
    /// The default local engine; Parakeet when the settings are unreadable,
    /// since repairing them starts a new choice.
    pub fn local_engine(&self) -> LocalEngine {
        self.settings()
            .map_or(LocalEngine::Parakeet, |s| s.local_engine)
    }
    pub fn capture_provider(
        &self,
        credentials: impl FnOnce() -> Result<crate::config::Credentials, String>,
    ) -> Result<crate::capture::CaptureProvider, String> {
        self.0
            .as_ref()
            .map_err(Clone::clone)?
            .capture_provider(credentials)
    }
    /// Commit a user choice only after persistence succeeds; otherwise stay blocked.
    pub fn choose(&mut self, provider: Provider, path: &Path) -> Result<(), String> {
        let mut settings = self.0.as_ref().cloned().unwrap_or_default();
        settings.select(provider);
        settings.save(path)?;
        self.0 = Ok(settings);
        Ok(())
    }
}

/// Whether the managed Whisper runtime (`scripts/local-whisper.py --app
/// murmur`) has a selected model beside this settings file:
/// `<Application Support>/Murmur/speech.json` and
/// `<Application Support>/murmur/local-whisper/model.bin`.
fn whisper_runtime_installed(settings: &Path) -> bool {
    settings
        .parent()
        .and_then(Path::parent)
        .is_some_and(|support| support.join("murmur/local-whisper/model.bin").exists())
}
