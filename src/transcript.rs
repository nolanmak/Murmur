//! Commit final utterances in arrival order, replacing revisions by ID.
#[derive(Default)]
pub struct Transcript {
    entries: Vec<(String, u32, String, bool)>,
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
