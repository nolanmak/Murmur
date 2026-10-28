//! Murmur file persistence for the shared credential-free vocabulary schema.
pub use fotw_stt::vocabulary::{Correction, MAX_PROFILE_BYTES, Profile, Snippet, validate_hints};
pub use fotw_stt::vocabulary_matcher::VocabularyMatcher;
use std::path::{Path, PathBuf};
/// Murmur's storage location and guarded writer policy; the wire schema is shared.
pub trait ProfileFile: Sized {
    fn path() -> Result<PathBuf, String>;
    fn read(path: &Path) -> Result<Self, String>;
    fn load(path: &Path) -> Result<Self, String>;
    fn save(&self, path: &Path) -> Result<(), String>;
    fn update(
        path: &Path,
        edit: impl FnOnce(&mut Self) -> Result<(), String>,
    ) -> Result<(), String>;
}
impl ProfileFile for Profile {
    fn path() -> Result<PathBuf, String> {
        Ok(
            PathBuf::from(std::env::var_os("HOME").ok_or("Cannot locate home directory")?)
                .join("Library/Application Support/Murmur/vocabulary.json"),
        )
    }
    fn read(path: &Path) -> Result<Self, String> {
        use std::io::Read;
        let file = std::fs::File::open(path).map_err(|_| "Cannot read vocabulary profile")?;
        let mut bytes = Vec::new();
        file.take((MAX_PROFILE_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|_| "Cannot read vocabulary profile")?;
        Self::parse(&bytes)
    }
    fn load(path: &Path) -> Result<Self, String> {
        match std::fs::metadata(path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            _ => Self::read(path),
        }
    }
    /// Replace a validated profile. Imports intentionally replace all fields.
    fn save(&self, path: &Path) -> Result<(), String> {
        self.validate()?;
        let _lock = lock_profile(path)?;
        persist(self, path)
    }
    /// Read, modify and atomically save while excluding cooperating writers.
    /// Refuse contention instead of blocking the native UI on another process.
    fn update(
        path: &Path,
        edit: impl FnOnce(&mut Self) -> Result<(), String>,
    ) -> Result<(), String> {
        let _lock = lock_profile(path)?;
        let mut profile = Self::load(path)?;
        edit(&mut profile)?;
        profile.validate()?;
        persist(&profile, path)
    }
}
fn persist(profile: &Profile, path: &Path) -> Result<(), String> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent).map_err(|_| "Cannot create vocabulary directory")?;
    let mut file =
        tempfile::NamedTempFile::new_in(parent).map_err(|_| "Cannot create vocabulary file")?;
    serde_json::to_writer_pretty(&mut file, profile)
        .map_err(|_| "Cannot write vocabulary profile")?;
    file.persist(path)
        .map_err(|_| "Cannot save vocabulary profile")?;
    Ok(())
}

struct ProfileLock(std::fs::File);
impl Drop for ProfileLock {
    fn drop(&mut self) {
        // Closing alone can leave the lock held by a descriptor inherited by
        // a concurrently spawning child. Ownership ends at this writer guard.
        let _ = self.0.unlock();
    }
}

fn lock_profile(path: &Path) -> Result<ProfileLock, String> {
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
    Ok(ProfileLock(lock))
}

#[cfg(test)]
mod tests {
    #[test]
    fn releasing_profile_owner_unlocks_even_if_an_inherited_descriptor_still_exists() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("profile.json");
        let owner = super::lock_profile(&path).unwrap();
        let inherited = owner.0.try_clone().unwrap();
        drop(owner);
        let next = super::lock_profile(&path);
        assert!(
            next.is_ok(),
            "a released profile owner must not remain locked through an inherited descriptor"
        );
        drop(inherited);
    }
}
