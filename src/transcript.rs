//! Commit final utterances in arrival order, replacing revisions by ID.
#[derive(Default)]
pub struct Transcript {
    entries: Vec<(String, u32, String, bool)>,
}

/// The latest finalized utterance available for delivery or recovery.
/// Starting a new recording invalidates the previous utterance immediately,
/// including when that attempt later fails preflight or microphone startup.
#[derive(Default, Debug, PartialEq, Eq)]
pub struct Latest {
    text: String,
}

impl Latest {
    pub fn begin(&mut self) {
        self.text.clear();
    }

    pub fn set(&mut self, text: impl Into<String>) {
        self.text = text.into();
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }
}

impl AsRef<str> for Latest {
    fn as_ref(&self) -> &str {
        &self.text
    }
}

impl std::ops::Deref for Latest {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        &self.text
    }
}

impl From<&str> for Latest {
    fn from(text: &str) -> Self {
        Self { text: text.into() }
    }
}
impl Transcript {
    pub fn accept(&mut self, id: &str, revision: u32, text: &str, finalized: bool) {
        if let Some(entry) = self.entries.iter_mut().find(|e| e.0 == id) {
            if revision > entry.1 || (revision == entry.1 && finalized && !entry.3) {
                *entry = (id.into(), revision, text.into(), finalized);
            }
        } else {
            self.entries
                .push((id.into(), revision, text.into(), finalized));
        }
    }
    pub fn text(&self) -> String {
        self.entries
            .iter()
            .filter(|e| e.3 && !e.2.trim().is_empty())
            .map(|e| e.2.trim())
            .collect::<Vec<_>>()
            .join(" ")
    }
}
