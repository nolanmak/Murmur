//! Field-scoped settings edits: merge unrelated changes, reject stale drafts.
use crate::vocabulary::{Profile, Snippet};
use std::path::Path;

pub struct Editor {
    pub baseline: Profile,
}
impl Editor {
    pub fn open(path: &Path) -> Result<Self, String> {
        Ok(Self {
            baseline: Profile::load(path)?,
        })
    }
    pub fn save_hints(&self, path: &Path, text: &str) -> Result<(), String> {
        let hints = text
            .lines()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect();
        Profile::update(path, |profile| {
            if profile.recognition_hints != self.baseline.recognition_hints {
                return Err("Recognition hints changed while this editor was open. Reopen the editor to load the latest version.".into());
            }
            profile.recognition_hints = hints;
            Ok(())
        })
    }
    pub fn save_snippet(
        &self,
        path: &Path,
        original: Option<&str>,
        snippet: Snippet,
    ) -> Result<(), String> {
        Profile::update(path, |profile| {
            if let Some(name) = original {
                let index = self.unchanged_snippet(profile, name)?;
                profile.snippets[index] = snippet;
            } else {
                profile.snippets.push(snippet);
            }
            Ok(())
        })
    }
    pub fn delete_snippet(&self, path: &Path, name: &str) -> Result<(), String> {
        Profile::update(path, |profile| {
            let index = self.unchanged_snippet(profile, name)?;
            profile.snippets.remove(index);
            Ok(())
        })
    }
    fn unchanged_snippet(&self, latest: &Profile, name: &str) -> Result<usize, String> {
        let before = self
            .baseline
            .snippets
            .iter()
            .find(|s| s.name == name)
            .ok_or("Snippet no longer exists. Reopen the editor.")?;
        latest.snippets.iter().position(|s| s == before)
            .ok_or_else(|| "This snippet changed while its editor was open. Reopen the editor to load the latest version.".into())
    }
}
