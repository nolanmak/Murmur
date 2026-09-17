//! Read-only credential resolution. Dotenv is parsed, never sourced by a shell.
use fotw_secrets::SecretString;
#[cfg(not(target_os = "linux"))]
use fotw_secrets::{KeyStore, OsKeyStore, Provider, SecretKey};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};
#[derive(Debug)]
pub struct Credentials {
    pub key: SecretString,
    pub source: &'static str,
}
pub fn resolve(
    vars: &HashMap<String, String>,
    path: Option<&Path>,
    fallback: impl FnOnce() -> Result<Option<String>, String>,
) -> Result<Credentials, String> {
    let process = vars
        .get("DEEPGRAM_API_KEY")
        .filter(|s| !s.trim().is_empty());
    if let Some(key) = process {
        return Ok(Credentials {
            key: SecretString::new(key.clone()),
            source: "environment",
        });
    }
    if let Some(path) = path {
        let mut entries = HashMap::new();
        for entry in dotenvy::from_path_iter(path)
            .map_err(|_| "Cannot read the configured .env file".to_string())?
        {
            let (k, v) = entry.map_err(|_| "Invalid .env syntax; values withheld".to_string())?;
            entries.insert(k, v);
        }
        if let Some(key) = entries
            .remove("DEEPGRAM_API_KEY")
            .filter(|s| !s.trim().is_empty())
        {
            return Ok(Credentials {
                key: SecretString::new(key),
                source: "dotenv",
            });
        }
    }
    match fallback()? {
        Some(key) if !key.trim().is_empty() => Ok(Credentials {
            key: SecretString::new(key),
            source: "FlyOnTheWall Keychain",
        }),
        _ => Err(
            "No Deepgram key. Configure DEEPGRAM_API_KEY or FlyOnTheWall's Keychain key.".into(),
        ),
    }
}
pub fn bundle_env(executable: &Path) -> Option<PathBuf> {
    let macos = executable.parent()?;
    let contents = macos.parent()?;
    let bundle = contents.parent()?;
    if macos.file_name()? != "MacOS"
        || contents.file_name()? != "Contents"
        || bundle.extension()? != "app"
    {
        return None;
    }
    let path = bundle.parent()?.join(".env");
    path.is_file().then_some(path)
}
pub fn load() -> Result<Credentials, String> {
    #[cfg(target_os = "linux")]
    {
        crate::platform::linux::credentials()
    }
    #[cfg(not(target_os = "linux"))]
    {
        load_legacy()
    }
}
#[cfg(not(target_os = "linux"))]
fn load_legacy() -> Result<Credentials, String> {
    let vars: HashMap<String, String> = std::env::vars().collect();
    let path = vars
        .get("FOTW_ENV_FILE")
        .filter(|s| !s.trim().is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::current_exe()
                .ok()
                .and_then(|exe| bundle_env(&exe))
        })
        .or_else(|| {
            let local = PathBuf::from(".env");
            local.exists().then_some(local)
        });
    resolve(&vars, path.as_deref(), || {
        let store = OsKeyStore::new().map_err(|_| "Cannot access macOS Keychain".to_string())?;
        store
            .get(SecretKey::ApiKey(Provider::Deepgram))
            .map(|s| Some(s.expose().to_owned()))
            .map_err(|_| {
                "Keychain read failed. Unlock it and allow this app if prompted.".to_string()
            })
    })
}
