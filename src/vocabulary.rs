//! Versioned, credential-free personal vocabulary and deliberate snippet cues.
use serde::{Deserialize, Serialize};
use std::{
    io::Read,
    path::{Path, PathBuf},
};

pub const MAX_PROFILE_BYTES: usize = 256 * 1024;
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Snippet {
    pub name: String,
    pub text: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    pub version: u32,
    #[serde(default = "default_language")]
    pub language: String,
    #[serde(default)]
    pub recognition_hints: Vec<String>,
    #[serde(default)]
    pub snippets: Vec<Snippet>,
}
fn default_language() -> String {
    "en".into()
}
impl Default for Profile {
    fn default() -> Self {
        Self {
            version: 1,
            language: default_language(),
            recognition_hints: Vec::new(),
            snippets: Vec::new(),
        }
    }
}
impl Profile {
    pub fn validate(&self) -> Result<(), String> {
        if self.version != 1 {
            return Err("Unsupported vocabulary schema version".into());
        }
        crate::language::validate_tag(&self.language)?;
        validate_hints(&self.recognition_hints)?;
        if self.snippets.len() > 128 {
            return Err("At most 128 snippets are supported".into());
        }
        let mut names = std::collections::HashSet::new();
        for snippet in &self.snippets {
            let name = &snippet.name;
            if name.is_empty()
                || name.len() > 80
                || name.trim() != name
                || name.split(' ').any(str::is_empty)
                || !name
                    .chars()
                    .all(|c| c.is_alphanumeric() || matches!(c, ' ' | '_' | '-'))
            {
                return Err("Snippet names must be 1–80 UTF-8 bytes of letters, numbers, spaces, hyphens or underscores".into());
            }
            if !names.insert(name.to_lowercase()) {
                return Err("Snippet names conflict ignoring case".into());
            }
            if snippet.text.is_empty()
                || snippet.text.len() > 16 * 1024
                || snippet
                    .text
                    .chars()
                    .any(|c| c.is_control() && !matches!(c, '\n' | '\t'))
            {
                return Err("Snippet text must be nonempty, at most 16 KiB, with no controls except newline and tab".into());
            }
        }
        if serde_json::to_vec_pretty(self)
            .map_err(|_| "Cannot encode vocabulary profile")?
            .len()
            > MAX_PROFILE_BYTES
        {
            return Err("Vocabulary profile exceeds 256 KiB".into());
        }
        Ok(())
    }
    pub fn expand(&self, text: &str) -> String {
        let cue = text.trim();
        let cue = cue
            .strip_suffix('.')
            .or_else(|| cue.strip_suffix('!'))
            .or_else(|| cue.strip_suffix('?'))
            .unwrap_or(cue)
            .to_lowercase();
        let Some(name) = cue.strip_prefix("insert snippet ") else {
            return text.to_owned();
        };
        self.snippets
            .iter()
            .find(|s| s.name.to_lowercase() == name)
            .map_or_else(|| text.to_owned(), |s| s.text.clone())
    }
    pub fn path() -> Result<PathBuf, String> {
        Ok(
            PathBuf::from(std::env::var_os("HOME").ok_or("Cannot locate home directory")?)
                .join("Library/Application Support/Murmur/vocabulary.json"),
        )
    }
    pub fn read(path: &Path) -> Result<Self, String> {
        let file = std::fs::File::open(path).map_err(|_| "Cannot read vocabulary profile")?;
        let mut bytes = Vec::new();
        file.take((MAX_PROFILE_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|_| "Cannot read vocabulary profile")?;
        if bytes.len() > MAX_PROFILE_BYTES {
            return Err("Vocabulary profile exceeds 256 KiB".into());
        }
        let profile: Self =
            serde_json::from_slice(&bytes).map_err(|_| "Invalid vocabulary profile schema")?;
        profile.validate()?;
        Ok(profile)
    }
    pub fn load(path: &Path) -> Result<Self, String> {
        match std::fs::metadata(path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            _ => Self::read(path),
        }
    }
    /// Replace a validated profile. Imports intentionally replace all fields.
    pub fn save(&self, path: &Path) -> Result<(), String> {
        self.validate()?;
        let _lock = lock_profile(path)?;
        self.persist(path)
    }
    /// Read, modify and atomically save while excluding cooperating writers.
    /// Refuse contention instead of blocking the native UI on another process.
    pub fn update(
        path: &Path,
        edit: impl FnOnce(&mut Self) -> Result<(), String>,
    ) -> Result<(), String> {
        let _lock = lock_profile(path)?;
        let mut profile = Self::load(path)?;
        edit(&mut profile)?;
        profile.validate()?;
        profile.persist(path)
    }
    fn persist(&self, path: &Path) -> Result<(), String> {
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        std::fs::create_dir_all(parent).map_err(|_| "Cannot create vocabulary directory")?;
        let mut file =
            tempfile::NamedTempFile::new_in(parent).map_err(|_| "Cannot create vocabulary file")?;
        serde_json::to_writer_pretty(&mut file, self)
            .map_err(|_| "Cannot write vocabulary profile")?;
        file.persist(path)
            .map_err(|_| "Cannot save vocabulary profile")?;
        Ok(())
    }
}

/// Conservative application limit in UTF-8 bytes, not a claimed tokenizer count.
/// Includes separators; Deepgram currently caps Nova-3 keyterms at 500 tokens.
pub fn validate_hints(hints: &[String]) -> Result<(), String> {
    if hints.len() > 64 || hints.iter().map(|h| h.len() + 1).sum::<usize>() > 500 {
        return Err(
            "Recognition hints exceed 64 terms or 500 UTF-8 bytes including separators".into(),
        );
    }
    let mut unique = std::collections::HashSet::new();
    for hint in hints {
        if hint.is_empty()
            || hint.trim() != hint
            || hint.len() > 128
            || hint.chars().any(char::is_control)
        {
            return Err("Recognition hints must be trimmed, nonempty, at most 128 UTF-8 bytes and contain no controls".into());
        }
        if !unique.insert(hint.to_lowercase()) {
            return Err("Recognition hints conflict ignoring case".into());
        }
    }
    Ok(())
}

fn lock_profile(path: &Path) -> Result<std::fs::File, String> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent).map_err(|_| "Cannot create vocabulary directory")?;
    let mut name = path
        .file_name()
        .ok_or("Invalid vocabulary path")?
        .to_os_string();
    name.push(".lock");
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(parent.join(name))
        .map_err(|_| "Cannot open vocabulary lock")?;
    lock.try_lock()
        .map_err(|_| "Vocabulary is being updated by another process. Try Save again.")?;
    Ok(lock)
}
