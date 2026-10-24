//! Native, field-scoped vocabulary forms. No recording or clipboard access.
use crate::{
    vocabulary::{Profile, Snippet},
    vocabulary_editor::Editor,
};
use muda::{MenuItem, Submenu};
use objc2::{MainThreadMarker, MainThreadOnly, rc::Retained};
use objc2_app_kit::{
    NSAccessibility, NSAlert, NSApplication, NSImage, NSScrollView, NSTextField, NSTextView, NSView,
};
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};
use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

#[cfg(debug_assertions)]
#[doc(hidden)]
pub mod qa;

#[cfg(debug_assertions)]
fn present(alert: &Retained<NSAlert>) -> isize {
    qa::present(alert)
}
#[cfg(not(debug_assertions))]
fn present(alert: &Retained<NSAlert>) -> isize {
    alert.runModal()
}

pub struct VocabularyMenu {
    pub menu: Submenu,
    snippets: Submenu,
    status: MenuItem,
    path: Result<PathBuf, String>,
    shown: Option<Profile>,
    refreshed: Instant,
}
impl VocabularyMenu {
    pub fn new(path: Result<PathBuf, String>) -> Result<Self, String> {
        let menu = Submenu::new("Vocabulary and Snippets", true);
        let snippets = Submenu::new("Edit snippets", true);
        let status = MenuItem::new("", false, None);
        menu.append(&MenuItem::with_id(
            "vocabulary:hints",
            "Recognition hints…",
            true,
            None,
        ))
        .map_err(|e| e.to_string())?;
        menu.append(&MenuItem::with_id(
            "vocabulary:add",
            "Add snippet…",
            true,
            None,
        ))
        .map_err(|e| e.to_string())?;
        menu.append(&snippets).map_err(|e| e.to_string())?;
        menu.append(&status).map_err(|e| e.to_string())?;
        let mut result = Self {
            menu,
            snippets,
            status,
            path,
            shown: None,
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
        self.refreshed = Instant::now();
        let profile = self
            .path
            .as_ref()
            .map_err(Clone::clone)
            .and_then(|path| Profile::load(path));
        match profile {
            Ok(profile) => {
                self.status.set_text(format!(
                    "{} hints · {} snippets · saved locally",
                    profile.recognition_hints.len(),
                    profile.snippets.len()
                ));
                if self.shown.as_ref() == Some(&profile) {
                    return;
                }
                while self.snippets.remove_at(0).is_some() {}
                if profile.snippets.is_empty() {
                    let _ = self
                        .snippets
                        .append(&MenuItem::new("No snippets yet", false, None));
                }
                for snippet in &profile.snippets {
                    let _ = self.snippets.append(&MenuItem::with_id(
                        format!("vocabulary:snippet:{}", snippet.name),
                        &snippet.name,
                        true,
                        None,
                    ));
                }
                self.shown = Some(profile);
            }
            Err(_) => {
                self.status
                    .set_text("Profile needs attention · existing file preserved");
                while self.snippets.remove_at(0).is_some() {}
                self.shown = None;
            }
        }
    }
    pub fn handles(id: &str) -> bool {
        id.starts_with("vocabulary:")
    }
    pub fn handle(&mut self, id: &str) -> Result<String, String> {
        let path = self.path.as_ref().map_err(Clone::clone)?;
        let editor = Editor::open(path)?;
        let mtm = MainThreadMarker::new().ok_or("Vocabulary editing requires the main thread")?;
        NSApplication::sharedApplication(mtm).activate();
        let result = match id {
            "vocabulary:hints" => edit_hints(path, &editor, mtm),
            "vocabulary:add" => edit_snippet(path, &editor, None, mtm),
            _ => {
                let name = id
                    .strip_prefix("vocabulary:snippet:")
                    .ok_or("Unknown vocabulary action")?;
                let snippet = editor
                    .baseline
                    .snippets
                    .iter()
                    .find(|s| s.name == name)
                    .ok_or("Snippet no longer exists. Reopen the menu.")?;
                edit_snippet(path, &editor, Some(snippet), mtm)
            }
        };
        self.refresh();
        Ok(result)
    }
}
fn rect(x: f64, y: f64, w: f64, h: f64) -> NSRect {
    NSRect::new(NSPoint::new(x, y), NSSize::new(w, h))
}
fn text_area(
    parent: &NSView,
    label: &str,
    value: &str,
    height: f64,
    mtm: MainThreadMarker,
) -> Retained<NSTextView> {
    let scroll =
        NSScrollView::initWithFrame(NSScrollView::alloc(mtm), rect(0.0, 0.0, 480.0, height));
    scroll.setHasVerticalScroller(true);
    let text = NSTextView::initWithFrame(NSTextView::alloc(mtm), rect(0.0, 0.0, 455.0, height));
    text.setRichText(false);
    text.setAllowsUndo(true);
    text.setEditable(true);
    text.setSelectable(true);
    text.setVerticallyResizable(true);
    text.setAutomaticQuoteSubstitutionEnabled(false);
    text.setAutomaticDashSubstitutionEnabled(false);
    text.setAutomaticTextReplacementEnabled(false);
    text.setAutomaticSpellingCorrectionEnabled(false);
    text.setAccessibilityLabel(Some(&NSString::from_str(label)));
    text.setString(&NSString::from_str(value));
    scroll.setDocumentView(Some(&text));
    parent.addSubview(&scroll);
    text
}
fn dialog(title: &str, instructions: &str, mtm: MainThreadMarker) -> Retained<NSAlert> {
    let alert = NSAlert::new(mtm);
    alert.setMessageText(&NSString::from_str(title));
    let icon = NSImage::imageWithSystemSymbolName_accessibilityDescription(
        &NSString::from_str("waveform"),
        Some(&NSString::from_str("Murmur vocabulary")),
    );
    // SAFETY: the immutable system-symbol image is created and retained on this main thread.
    unsafe { alert.setIcon(icon.as_deref()) };
    let window = alert.window();
    window.setAccessibilityElement(true);
    window.setAccessibilityRole(Some(&NSString::from_str("AXWindow")));
    window.setAccessibilitySubrole(Some(&NSString::from_str("AXDialog")));
    alert.setInformativeText(&NSString::from_str(instructions));
    alert.addButtonWithTitle(&NSString::from_str("Save"));
    alert.addButtonWithTitle(&NSString::from_str("Cancel"));
    alert
}
fn edit_hints(path: &Path, editor: &Editor, mtm: MainThreadMarker) -> String {
    let instructions = "One name or term per line. Blank lines are ignored. Hints apply to the next recording. Deepgram supports hints; the current local adapter does not use them.";
    let alert = dialog("Recognition hints", instructions, mtm);
    let content = NSView::initWithFrame(NSView::alloc(mtm), rect(0.0, 0.0, 480.0, 200.0));
    let text = text_area(
        &content,
        "Recognition hints",
        &editor.baseline.recognition_hints.join("\n"),
        200.0,
        mtm,
    );
    alert.setAccessoryView(Some(&content));
    alert.window().setInitialFirstResponder(Some(&text));
    loop {
        if present(&alert) != 1000 {
            return "Vocabulary changes cancelled".into();
        }
        match editor.save_hints(path, &text.string().to_string()) {
            Ok(()) => return "Recognition hints saved · applies to the next recording".into(),
            Err(error) => {
                alert.setInformativeText(&NSString::from_str(&format!("{instructions}\n\n{error}")))
            }
        }
    }
}
fn edit_snippet(
    path: &Path,
    editor: &Editor,
    original: Option<&Snippet>,
    mtm: MainThreadMarker,
) -> String {
    let instructions = "Say a complete utterance: insert snippet NAME. Text is inserted exactly as written. Changes apply to the next recording.";
    let alert = dialog(
        if original.is_some() {
            "Edit snippet"
        } else {
            "Add snippet"
        },
        instructions,
        mtm,
    );
    if original.is_some() {
        alert.addButtonWithTitle(&NSString::from_str("Delete snippet…"));
    }
    let content = NSView::initWithFrame(NSView::alloc(mtm), rect(0.0, 0.0, 480.0, 265.0));
    let name_label = NSTextField::labelWithString(&NSString::from_str("Snippet name"), mtm);
    name_label.setFrame(rect(0.0, 240.0, 480.0, 20.0));
    content.addSubview(&name_label);
    let name = NSTextField::initWithFrame(NSTextField::alloc(mtm), rect(0.0, 210.0, 480.0, 26.0));
    name.setAccessibilityLabel(Some(&NSString::from_str("Snippet name")));
    name.setStringValue(&NSString::from_str(original.map_or("", |s| &s.name)));
    content.addSubview(&name);
    let text_label = NSTextField::labelWithString(&NSString::from_str("Snippet text"), mtm);
    text_label.setFrame(rect(0.0, 182.0, 480.0, 22.0));
    content.addSubview(&text_label);
    let text = text_area(
        &content,
        "Snippet text",
        original.map_or("", |s| &s.text),
        175.0,
        mtm,
    );
    alert.setAccessoryView(Some(&content));
    alert.window().setInitialFirstResponder(Some(&name));
    loop {
        let response = present(&alert);
        let result = if response == 1000 {
            editor.save_snippet(
                path,
                original.map(|s| s.name.as_str()),
                Snippet {
                    name: name.stringValue().to_string().trim().to_owned(),
                    text: text.string().to_string(),
                },
            )
        } else if response == 1002 && original.is_some() {
            let confirm = NSAlert::new(mtm);
            confirm.setAlertStyle(objc2_app_kit::NSAlertStyle::Warning);
            let icon = NSImage::imageWithSystemSymbolName_accessibilityDescription(
                &NSString::from_str("trash"),
                Some(&NSString::from_str("Delete snippet")),
            );
            // SAFETY: immutable system-symbol image retained on the main thread.
            unsafe { confirm.setIcon(icon.as_deref()) };
            confirm.setMessageText(&NSString::from_str("Delete this snippet?"));
            confirm.setInformativeText(&NSString::from_str(
                "The saved snippet will be removed. Cancel keeps it unchanged.",
            ));
            confirm.addButtonWithTitle(&NSString::from_str("Cancel"));
            confirm.addButtonWithTitle(&NSString::from_str("Delete"));
            if present(&confirm) != 1001 {
                continue;
            }
            editor.delete_snippet(path, &original.unwrap().name)
        } else {
            return "Snippet changes cancelled".into();
        };
        match result {
            Ok(()) => {
                return if response == 1002 {
                    "Snippet deleted"
                } else {
                    "Snippet saved · applies to the next recording"
                }
                .into();
            }
            Err(error) => {
                alert.setInformativeText(&NSString::from_str(&format!("{instructions}\n\n{error}")))
            }
        }
    }
}

/// Standard responder actions for native text fields, including keyboard shortcuts.
pub fn edit_menu() -> Result<Submenu, String> {
    use muda::PredefinedMenuItem;
    let menu = Submenu::new("Edit", true);
    for item in [
        PredefinedMenuItem::undo(None),
        PredefinedMenuItem::redo(None),
        PredefinedMenuItem::cut(None),
        PredefinedMenuItem::copy(None),
        PredefinedMenuItem::paste(None),
        PredefinedMenuItem::select_all(None),
    ] {
        menu.append(&item).map_err(|e| e.to_string())?;
    }
    Ok(menu)
}
