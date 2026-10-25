//! Credential-free cleanup settings and a separate global network policy.
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    pub version: u8,
    pub enabled: bool,
    pub endpoint: String,
    pub model: String,
    pub credential_env: String,
    pub deadline_ms: u64,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            version: 1,
            enabled: false,
            endpoint: String::new(),
            model: String::new(),
            credential_env: "MURMUR_CLEANUP_API_KEY".into(),
            deadline_ms: 1500,
        }
    }
}
impl Settings {
    pub fn validate(&self) -> Result<(), String> {
        if self.version != 1
            || !(100..=5000).contains(&self.deadline_ms)
            || self.credential_env.is_empty()
            || self.credential_env.len() > 64
            || !self
                .credential_env
                .bytes()
                .enumerate()
                .all(|(i, b)| b == b'_' || b.is_ascii_uppercase() || (i > 0 && b.is_ascii_digit()))
        {
            return Err("Invalid cleanup schema, credential variable or deadline".into());
        }
        if self.endpoint.is_empty() && self.model.is_empty() && !self.enabled {
            return Ok(());
        }
        if self.endpoint.len() > 4096 {
            return Err("Cleanup endpoint exceeds size limit".into());
        }
        let url = reqwest::Url::parse(&self.endpoint).map_err(|_| "Invalid cleanup endpoint")?;
        let loopback = matches!(url.host_str(), Some("127.0.0.1" | "[::1]"));
        if (url.scheme() != "https" && !(url.scheme() == "http" && loopback))
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || !url.path().ends_with("/chat/completions")
            || self.model.trim() != self.model
            || self.model.is_empty()
            || self.model.len() > 128
            || self.model.chars().any(char::is_control)
        {
            return Err("Cleanup requires an explicit HTTPS chat/completions endpoint and model; HTTP is allowed only for literal loopback addresses".into());
        }
        Ok(())
    }
    pub fn path() -> Result<PathBuf, String> {
        path("cleanup.json")
    }
    pub fn load(file: &Path) -> Result<Self, String> {
        let settings: Self = load(file)?;
        settings.validate()?;
        Ok(settings)
    }
    pub fn save(&self, file: &Path) -> Result<(), String> {
        self.validate()?;
        let _lock = lock(file)?;
        save(file, self)
    }
    /// Toggle only the reviewed configuration; a changed destination needs review.
    pub fn set_enabled(file: &Path, expected: &Self, enabled: bool) -> Result<(), String> {
        let _lock = lock(file)?;
        let mut current = Self::load(file)?;
        if &current != expected {
            return Err("Cleanup settings changed; review the destination again".into());
        }
        current.enabled = enabled;
        current.validate()?;
        save(file, &current)
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NetworkPolicy {
    pub version: u8,
    pub offline_all: bool,
}
impl Default for NetworkPolicy {
    fn default() -> Self {
        Self {
            version: 1,
            offline_all: false,
        }
    }
}
impl NetworkPolicy {
    pub fn path() -> Result<PathBuf, String> {
        path("network-policy.json")
    }
    pub fn load(file: &Path) -> Result<Self, String> {
        let value: Self = load(file).map_err(|_| "Invalid network policy; no audio sent")?;
        if value.version != 1 {
            return Err("Invalid network policy; no audio sent".into());
        }
        Ok(value)
    }
    pub fn save(&self, file: &Path) -> Result<(), String> {
        if self.version != 1 {
            return Err("Invalid network policy".into());
        }
        let _lock = lock(file)?;
        save(file, self)
    }
}
struct SettingsLock(std::fs::File);
impl Drop for SettingsLock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}
fn lock(file: &Path) -> Result<SettingsLock, String> {
    let dir = file.parent().ok_or("Invalid settings directory")?;
    std::fs::create_dir_all(dir).map_err(|_| "Cannot create settings directory")?;
    let mut name = file
        .file_name()
        .ok_or("Invalid settings file")?
        .to_os_string();
    name.push(".lock");
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join(name))
        .map_err(|_| "Cannot lock settings")?;
    file.try_lock()
        .map_err(|_| "Settings are being edited; try again")?;
    Ok(SettingsLock(file))
}

/// CLI settings actions never load a credential or contact a provider.
pub fn command(
    args: &[String],
    settings_file: &Path,
    policy_file: &Path,
) -> Result<String, String> {
    match args.first().map(String::as_str) {
        Some("configure") if args.len() == 4 => {
            let settings = Settings { endpoint:args[1].clone(),model:args[2].clone(),credential_env:args[3].clone(),..Default::default() };
            settings.save(settings_file)?;
            Ok("Cleanup provider saved and remains off. Review its destination, then use cleanup on to allow sending the next utterance text.".into())
        }
        Some("on" | "off") if args.len() == 1 => {
            let settings = Settings::load(settings_file)?;
            let enabled = args[0] == "on";
            if enabled && NetworkPolicy::load(policy_file)?.offline_all { return Err("Offline-all blocks hosted cleanup".into()); }
            Settings::set_enabled(settings_file,&settings,enabled)?;
            Ok(if enabled { format!("Cleanup enabled for {} at {}. Only utterance text is sent; missing credentials or failure retain raw text. Applies to the next dictation.",settings.model,settings.endpoint) } else { "Cleanup off. Raw transcription and local snippets remain available.".into() })
        }
        Some("status") if args.len() == 1 => {
            let settings=Settings::load(settings_file)?;let policy=NetworkPolicy::load(policy_file)?;
            Ok(format!("Cleanup: {}; model: {}; endpoint: {}; credential variable: {} (value never loaded). Offline-all: {}.",if settings.enabled {"enabled"} else {"off"},settings.model,settings.endpoint,settings.credential_env,policy.offline_all))
        }
        Some("offline-all" | "configured") if args.len() == 1 => {
            let offline_all=args[0]=="offline-all";
            NetworkPolicy {version:1,offline_all}.save(policy_file)?;
            Ok(if offline_all {"Offline-all saved: the next dictation uses local speech and bypasses cleanup. Cancel current dictation before changing policy.".into()} else {"Configured speech providers restored. Cleanup remains separately opt-in. Applies to the next dictation.".into()})
        }
        _ => Err("Usage: murmur cleanup configure CHAT_COMPLETIONS_URL MODEL CREDENTIAL_VARIABLE | on | off | status; murmur privacy offline-all | configured".into())
    }
}
fn path(name: &str) -> Result<PathBuf, String> {
    Ok(
        PathBuf::from(std::env::var_os("HOME").ok_or("Cannot locate settings directory")?)
            .join("Library/Application Support/Murmur")
            .join(name),
    )
}
fn load<T: serde::de::DeserializeOwned + Default>(file: &Path) -> Result<T, String> {
    use std::io::Read;
    match std::fs::File::open(file) {
        Ok(file) => {
            let mut bytes = Vec::new();
            file.take(16385)
                .read_to_end(&mut bytes)
                .map_err(|_| "Cannot read cleanup settings")?;
            if bytes.len() > 16384 {
                return Err("Cleanup settings too large".into());
            }
            serde_json::from_slice(&bytes).map_err(|_| "Invalid cleanup settings".into())
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(T::default()),
        Err(_) => Err("Cannot read cleanup settings".into()),
    }
}
fn save<T: Serialize>(path: &Path, value: &T) -> Result<(), String> {
    let dir = path.parent().ok_or("Invalid settings directory")?;
    std::fs::create_dir_all(dir).map_err(|_| "Cannot create settings directory")?;
    let mut temp =
        tempfile::NamedTempFile::new_in(dir).map_err(|_| "Cannot create settings file")?;
    serde_json::to_writer_pretty(&mut temp, value).map_err(|_| "Cannot write settings")?;
    temp.as_file()
        .sync_all()
        .map_err(|_| "Cannot sync settings")?;
    temp.persist(path).map_err(|_| "Cannot save settings")?;
    #[cfg(unix)]
    std::fs::File::open(dir)
        .and_then(|dir| dir.sync_all())
        .map_err(|_| "Cannot sync settings directory")?;
    Ok(())
}
