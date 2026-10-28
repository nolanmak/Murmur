//! Persisted provider settings, independent of cloud credentials.
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum Provider {
    #[default]
    Deepgram,
    LocalWhisper,
    DeepgramThenLocal,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    pub provider: Provider,
    pub local_endpoint: String,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            provider: Provider::Deepgram,
            local_endpoint: "http://127.0.0.1:17863".into(),
        }
    }
}
impl Settings {
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
            Provider::DeepgramThenLocal => {
                let local = crate::local_stt::LocalEndpoint::new(&self.local_endpoint)?;
                match credentials() {
                    Ok(credentials) => Ok(crate::capture::CaptureProvider::DeepgramThenLocal {
                        key: credentials.key.expose().into(),
                        endpoint: fotw_stt::DeepgramEndpoint::production(),
                        local,
                    }),
                    Err(_) => Ok(crate::capture::CaptureProvider::Local(local)),
                }
            }
            Provider::LocalWhisper => Ok(crate::capture::CaptureProvider::Local(
                crate::local_stt::LocalEndpoint::new(&self.local_endpoint)?,
            )),
        }
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
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(_) => Err("Cannot read speech settings".into()),
        }
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
            return Ok(crate::capture::CaptureProvider::Local(
                crate::local_stt::LocalEndpoint::new(&settings.local_endpoint)?,
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
        settings.provider = provider;
        settings.save(path)?;
        self.0 = Ok(settings);
        Ok(())
    }
}
