//! The app's one `fotw-speechd` helper process, shared by Local Parakeet
//! (#101) and local streaming (#100), so their models load into one process
//! and one restart policy covers both. The helper client replays every load
//! and warmup either engine asked for after a crash.
use std::sync::{Arc, Mutex, OnceLock};

use fotw_speechd_client::{ClientError, SpeechdClient, SpeechdConfig};

/// Why the helper could not be started.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StartError {
    /// Not bundled, or it cannot run in this build.
    Unavailable(String),
    /// Anything else.
    Failed(String),
}

/// Starts the helper on first use and hands out clients of it.
pub struct Helper {
    config: Result<SpeechdConfig, String>,
    client: tokio::sync::Mutex<Option<SpeechdClient>>,
    current: Mutex<Option<SpeechdClient>>,
}

static SHARED: OnceLock<Arc<Helper>> = OnceLock::new();

impl Helper {
    /// Any helper; tests pass the model-free fake.
    pub fn new(config: Result<SpeechdConfig, String>) -> Arc<Self> {
        let config = config.map(|mut config| {
            config.client_name = concat!("murmur/", env!("CARGO_PKG_VERSION")).into();
            config
        });
        Arc::new(Self {
            config,
            client: tokio::sync::Mutex::new(None),
            current: Mutex::new(None),
        })
    }

    /// The app's helper: the bundled one (`Contents/Helpers/fotw-speechd`, or
    /// `FOTW_SPEECHD_PATH`) over Murmur's models root. Creating it starts
    /// nothing.
    pub fn shared() -> Arc<Self> {
        SHARED
            .get_or_init(|| {
                Self::new(
                    match (fotw_speechd_client::locate_helper(), models_root()) {
                        (Some(program), Ok(root)) => Ok(SpeechdConfig::new(program, root)),
                        (None, _) => Err(
                            "this build does not include the fotw-speechd helper; rebuild with MURMUR_SPEECHD=1 ./scripts/build-app.sh, or choose Local Whisper"
                                .to_string(),
                        ),
                        (_, Err(error)) => Err(error),
                    },
                )
            })
            .clone()
    }

    /// The running helper, started (or restarted after its budget was spent)
    /// if needed.
    pub async fn client(&self) -> Result<SpeechdClient, StartError> {
        let mut client = self.client.lock().await;
        if let Some(existing) = client.as_ref() {
            if !existing.status().failed {
                return Ok(existing.clone());
            }
            // The restart budget is spent; start counting again.
            existing.reset().await;
            if !existing.status().failed {
                return Ok(existing.clone());
            }
        }
        let config = self.config.clone().map_err(StartError::Unavailable)?;
        let started = SpeechdClient::start(config).await.map_err(|e| match e {
            ClientError::Spawn(reason) => StartError::Unavailable(reason),
            ClientError::VersionMismatch { .. } => {
                StartError::Unavailable(format!("{e}; rebuild Murmur so its helper matches"))
            }
            other => StartError::Failed(other.to_string()),
        })?;
        *self.current.lock().unwrap_or_else(|e| e.into_inner()) = Some(started.clone());
        *client = Some(started.clone());
        Ok(started)
    }

    /// The running helper, if any; never starts one (unloading needs none).
    pub fn running(&self) -> Option<SpeechdClient> {
        let client = self.current.lock().unwrap_or_else(|e| e.into_inner());
        client.clone().filter(|c| !c.status().failed)
    }

    /// Which helper process serves now (the client's spawn generation), if
    /// one is running; a restart changes it.
    pub fn instance(&self) -> Option<u64> {
        let client = self.current.lock().unwrap_or_else(|e| e.into_inner());
        client
            .as_ref()
            .map(SpeechdClient::status)
            .filter(|status| !status.failed)
            .map(|status| status.generation)
    }
}

/// Where installed models live; `MURMUR_SPEECHD_MODELS` overrides it.
pub fn models_root() -> Result<std::path::PathBuf, String> {
    if let Some(root) = std::env::var_os("MURMUR_SPEECHD_MODELS").filter(|v| !v.is_empty()) {
        return Ok(root.into());
    }
    Ok(
        std::path::PathBuf::from(std::env::var_os("HOME").ok_or("Cannot locate home directory")?)
            .join("Library/Application Support/Murmur/models"),
    )
}
