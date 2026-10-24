//! Native language menu. Saving reloads the profile instead of overwriting a stale copy.
use crate::{language, language_catalog, vocabulary::Profile};
use muda::{CheckMenuItem, MenuItem, Submenu};
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

pub struct LanguageMenu {
    pub menu: Submenu,
    items: Vec<(&'static str, CheckMenuItem)>,
    path: Result<PathBuf, String>,
    refreshed: Instant,
}
impl LanguageMenu {
    pub fn new(path: Result<PathBuf, String>) -> Result<Self, String> {
        let menu = Submenu::with_id("language_menu", "Language", true);
        let mut items = Vec::new();
        let mut append = |parent: &Submenu, code: &'static str| -> Result<(), String> {
            let item = CheckMenuItem::with_id(
                format!("language:{code}"),
                language_catalog::label(code),
                true,
                false,
                None,
            );
            parent.append(&item).map_err(|e| e.to_string())?;
            items.push((code, item));
            Ok(())
        };
        for &code in language_catalog::COMMON {
            append(&menu, code)?;
        }
        let codes = language_catalog::codes();
        for (title, first, last) in [
            ("A–F", 'A', 'F'),
            ("G–L", 'G', 'L'),
            ("M–R", 'M', 'R'),
            ("S–Z", 'S', 'Z'),
        ] {
            let group = Submenu::new(title, true);
            for &code in &codes {
                let initial = language_catalog::label(code).chars().next().unwrap_or(' ');
                if (first..=last).contains(&initial) && !language_catalog::COMMON.contains(&code) {
                    append(&group, code)?;
                }
            }
            menu.append(&group).map_err(|e| e.to_string())?;
        }
        menu.append(&MenuItem::new(
            "Support depends on the transcription model",
            false,
            None,
        ))
        .map_err(|e| e.to_string())?;
        let mut result = Self {
            menu,
            items,
            path,
            refreshed: Instant::now(),
        };
        result.refresh();
        Ok(result)
    }

    pub fn refresh_if_due(&mut self) {
        if self.refreshed.elapsed() >= Duration::from_secs(1) {
            self.refresh();
        }
    }

    pub fn refresh(&mut self) {
        let selected = self
            .path
            .as_ref()
            .map_err(Clone::clone)
            .and_then(|path| Profile::load(path))
            .map(|profile| profile.language);
        self.menu.set_text(match &selected {
            Ok(code) => format!("Language: {}", language_catalog::label(code)),
            Err(_) => "Language: profile needs attention".into(),
        });
        for (code, item) in &self.items {
            let checked = selected.as_deref() == Ok(*code);
            if item.is_checked() != checked {
                item.set_checked(checked);
            }
        }
        self.refreshed = Instant::now();
    }

    pub fn handle(&mut self, id: &str) -> Option<Result<String, String>> {
        let code = id.strip_prefix("language:")?;
        let result = if self.items.iter().any(|(candidate, _)| *candidate == code) {
            self.path
                .as_ref()
                .map_err(Clone::clone)
                .and_then(|path| language::save_choice(path, code))
                .map(|()| {
                    format!(
                        "Language saved: {} · applies to the next recording",
                        language_catalog::label(code)
                    )
                })
        } else {
            Err("Unknown language menu choice".into())
        };
        // Clicking an already checked item may toggle it off in AppKit. Restore
        // radio semantics even when saving fails or the selection did not change.
        self.refresh();
        Some(result)
    }
}
