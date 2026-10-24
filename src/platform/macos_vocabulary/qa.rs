//! Owned AppKit modal fixtures. No Shell, event tap, clipboard or user profile.
use super::*;
use block2::RcBlock;
use objc2_app_kit::{
    NSApplicationActivationPolicy, NSBitmapImageFileType, NSModalPanelRunLoopMode,
};
use objc2_foundation::{NSDictionary, NSRunLoop, NSTimer};
use std::{cell::RefCell, collections::VecDeque, ptr::NonNull, rc::Rc};

struct Step {
    title: &'static str,
    button: usize,
    fields: Vec<(&'static str, &'static str)>,
    error: Option<&'static str>,
    snapshot: &'static str,
}
struct Plan {
    steps: VecDeque<Step>,
    output: PathBuf,
    failure: Option<String>,
}
thread_local! {
    static PLAN: RefCell<Option<Rc<RefCell<Plan>>>> = const { RefCell::new(None) };
}
struct Reset;
impl Drop for Reset {
    fn drop(&mut self) {
        PLAN.with(|slot| slot.borrow_mut().take());
    }
}

fn field(view: &NSView, label: &str, value: &str) -> bool {
    if let Some(text) = view.downcast_ref::<NSTextView>()
        && text
            .accessibilityLabel()
            .as_deref()
            .map(NSString::to_string)
            .as_deref()
            == Some(label)
    {
        text.setString(&NSString::from_str(value));
        return true;
    }
    if let Some(text) = view.downcast_ref::<NSTextField>()
        && text.isEditable()
        && text
            .accessibilityLabel()
            .as_deref()
            .map(NSString::to_string)
            .as_deref()
            == Some(label)
    {
        text.setStringValue(&NSString::from_str(value));
        return true;
    }
    view.subviews()
        .iter()
        .any(|child| field(&child, label, value))
}
fn snapshot(alert: &NSAlert, output: &Path) -> Result<(), String> {
    let content = alert.window().contentView().ok_or("Missing dialog view")?;
    // SAFETY: this is the NSAlert-owned NSView hierarchy; its parent is an NSView.
    let view = unsafe { content.superview() }.unwrap_or(content);
    view.layoutSubtreeIfNeeded();
    view.displayIfNeeded();
    let bounds = view.bounds();
    let bitmap = view
        .bitmapImageRepForCachingDisplayInRect(bounds)
        .ok_or("Cannot cache dialog")?;
    view.cacheDisplayInRect_toBitmapImageRep(bounds, &bitmap);
    // SAFETY: correctly typed empty encoding properties, as in the pill preview.
    let data = unsafe {
        bitmap.representationUsingType_properties(NSBitmapImageFileType::PNG, &NSDictionary::new())
    }
    .ok_or("Cannot encode dialog")?;
    if !data.writeToFile_atomically(&NSString::from_str(&output.to_string_lossy()), true) {
        return Err("Cannot write dialog preview".into());
    }
    Ok(())
}
fn drive(alert: &NSAlert, step: &Step, output: &Path) -> Result<(), String> {
    if alert.messageText().to_string() != step.title {
        return Err("Unexpected modal dialog".into());
    }
    if let Some(error) = step.error
        && !alert.informativeText().to_string().contains(error)
    {
        return Err(format!("Missing validation error: {error}"));
    }
    for (label, value) in &step.fields {
        if !alert
            .accessoryView()
            .is_some_and(|view| field(&view, label, value))
        {
            return Err(format!("Missing native field: {label}"));
        }
    }
    snapshot(alert, &output.join(format!("{}.png", step.snapshot)))?;
    let button = alert
        .buttons()
        .iter()
        .nth(step.button)
        .ok_or("Missing modal button")?;
    // SAFETY: NSAlert configures this native button's target/action; it is
    // retained and invoked only on the main thread with an allowed nil sender.
    unsafe { button.performClick(None) };
    Ok(())
}
pub(super) fn present(alert: &Retained<NSAlert>) -> isize {
    let Some(plan) = PLAN.with(|slot| slot.borrow().clone()) else {
        return alert.runModal();
    };
    let Some(step) = plan.borrow_mut().steps.pop_front() else {
        plan.borrow_mut().failure = Some("Unexpected extra dialog".into());
        return -1001;
    };
    alert.window().setAlphaValue(0.0);
    let owned = alert.clone();
    let driven = plan.clone();
    let block = RcBlock::new(move |_: NonNull<NSTimer>| {
        let output = driven.borrow().output.clone();
        if let Err(error) = drive(&owned, &step, &output) {
            driven.borrow_mut().failure = Some(error);
            if let Some(mtm) = MainThreadMarker::new() {
                NSApplication::sharedApplication(mtm).abortModal();
            }
        }
    });
    let expired = plan.clone();
    let deadline = RcBlock::new(move |_: NonNull<NSTimer>| {
        expired.borrow_mut().failure = Some("Owned modal driver timed out".into());
        if let Some(mtm) = MainThreadMarker::new() {
            NSApplication::sharedApplication(mtm).abortModal();
        }
    });
    // SAFETY: these private timers are registered only on the main run loop's
    // modal mode. Their AppKit/Rc captures never cross threads or escape here.
    let (timer, timeout) = unsafe {
        let timer = NSTimer::timerWithTimeInterval_repeats_block(0.02, false, &block);
        let timeout = NSTimer::timerWithTimeInterval_repeats_block(5.0, false, &deadline);
        NSRunLoop::mainRunLoop().addTimer_forMode(&timer, NSModalPanelRunLoopMode);
        NSRunLoop::mainRunLoop().addTimer_forMode(&timeout, NSModalPanelRunLoopMode);
        (timer, timeout)
    };
    let response = alert.runModal();
    timer.invalidate();
    timeout.invalidate();
    alert.window().orderOut(None);
    response
}
fn ensure(ok: bool, message: &str) -> Result<(), String> {
    if ok { Ok(()) } else { Err(message.into()) }
}
fn checked(menu: &muda::Submenu) -> Vec<String> {
    let mut result = Vec::new();
    for item in menu.items() {
        if let Some(check) = item.as_check_menuitem()
            && check.is_checked()
        {
            let id: &str = check.id().as_ref();
            result.push(id.to_owned());
        }
        if let Some(submenu) = item.as_submenu() {
            result.extend(checked(submenu));
        }
    }
    result
}

/// Explicit fixture directory only. Refuses an existing directory and never
/// initializes capture, permissions, global hotkeys or the user pasteboard.
pub fn run(output: &Path) -> Result<(), String> {
    let mtm = MainThreadMarker::new().ok_or("Run fixtures on the main thread")?;
    ensure(output.is_absolute(), "Pass an absolute fixture directory")?;
    ensure(!output.exists(), "Pass a new fixture directory")?;
    std::fs::create_dir(output).map_err(|e| e.to_string())?;
    let path = output.join("profile.json");
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    // A fixed appearance makes owned bitmap renders deterministic without
    // changing system settings or depending on compositor vibrancy.
    let appearance = objc2_app_kit::NSAppearance::appearanceNamed(unsafe {
        objc2_app_kit::NSAppearanceNameAqua
    });
    app.setAppearance(appearance.as_deref());
    app.finishLaunching();
    Profile::default().save(&path)?;
    let mut language = crate::platform::macos_language::LanguageMenu::new(Ok(path.clone()))?;
    for code in ["fr", "fr-CA", "fr-CA"] {
        language
            .handle(&format!("language:{code}"))
            .ok_or("Language route missing")??;
        ensure(
            checked(&language.menu) == [format!("language:{code}")],
            "Language checkmark mismatch",
        )?;
    }
    Profile::update(&path, |profile| {
        profile.recognition_hints = vec!["External hint".into()];
        Ok(())
    })?;
    language
        .handle("language:es")
        .ok_or("Language route missing")??;
    ensure(
        Profile::load(&path)?.recognition_hints == ["External hint"],
        "Language overwrote vocabulary",
    )?;

    let plan = Rc::new(RefCell::new(Plan {
        steps: VecDeque::new(),
        output: output.to_owned(),
        failure: None,
    }));
    PLAN.with(|slot| *slot.borrow_mut() = Some(plan.clone()));
    let _reset = Reset;
    let add = |title, button, fields, error, snapshot| {
        plan.borrow_mut().steps.push_back(Step {
            title,
            button,
            fields,
            error,
            snapshot,
        });
    };
    add(
        "Recognition hints",
        0,
        vec![("Recognition hints", "Café\nOpenAI")],
        None,
        "hints",
    );
    let editor = Editor::open(&path)?;
    ensure(
        edit_hints(&path, &editor, mtm).contains("saved"),
        "Native hints Save failed",
    )?;
    ensure(
        Profile::load(&path)?.recognition_hints == ["Café", "OpenAI"],
        "Native hints data mismatch",
    )?;
    let saved = std::fs::read(&path).map_err(|e| e.to_string())?;
    add(
        "Recognition hints",
        1,
        vec![("Recognition hints", "Discard draft")],
        None,
        "hints-cancel",
    );
    ensure(
        edit_hints(&path, &Editor::open(&path)?, mtm).contains("cancelled"),
        "Native hints Cancel failed",
    )?;
    ensure(
        std::fs::read(&path).map_err(|e| e.to_string())? == saved,
        "Cancel changed profile",
    )?;
    add(
        "Recognition hints",
        0,
        vec![("Recognition hints", "Term\nterm")],
        None,
        "hints-invalid-draft",
    );
    add(
        "Recognition hints",
        1,
        vec![],
        Some("conflict"),
        "hints-error",
    );
    edit_hints(&path, &Editor::open(&path)?, mtm);
    ensure(
        std::fs::read(&path).map_err(|e| e.to_string())? == saved,
        "Invalid hints changed profile",
    )?;
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(output.join("profile.json.lock"))
        .map_err(|e| e.to_string())?;
    lock.lock().map_err(|e| e.to_string())?;
    add(
        "Recognition hints",
        0,
        vec![("Recognition hints", "Locked draft")],
        None,
        "hints-locked-draft",
    );
    add(
        "Recognition hints",
        1,
        vec![],
        Some("another process"),
        "hints-locked",
    );
    edit_hints(&path, &Editor::open(&path)?, mtm);
    ensure(
        std::fs::read(&path).map_err(|e| e.to_string())? == saved,
        "Locked save changed profile",
    )?;
    drop(lock);
    let stale = Editor::open(&path)?;
    Profile::update(&path, |profile| {
        profile.recognition_hints = vec!["Newer external hint".into()];
        Ok(())
    })?;
    let newer = std::fs::read(&path).map_err(|e| e.to_string())?;
    add(
        "Recognition hints",
        0,
        vec![("Recognition hints", "Stale draft")],
        None,
        "hints-stale-draft",
    );
    add(
        "Recognition hints",
        1,
        vec![],
        Some("changed while"),
        "hints-stale",
    );
    edit_hints(&path, &stale, mtm);
    ensure(
        std::fs::read(&path).map_err(|e| e.to_string())? == newer,
        "Stale draft changed profile",
    )?;
    add(
        "Add snippet",
        0,
        vec![
            ("Snippet name", "signature"),
            ("Snippet text", "Hello\n\tWorld"),
        ],
        None,
        "snippet-add",
    );
    ensure(
        edit_snippet(&path, &Editor::open(&path)?, None, mtm).contains("saved"),
        "Native snippet Save failed",
    )?;
    let profile = Profile::load(&path)?;
    ensure(
        profile.snippets[0].text == "Hello\n\tWorld",
        "Native multiline snippet changed",
    )?;
    let editor = Editor::open(&path)?;
    add("Edit snippet", 2, vec![], None, "snippet-edit");
    add("Delete this snippet?", 0, vec![], None, "delete-cancel");
    add("Edit snippet", 1, vec![], None, "snippet-cancel");
    edit_snippet(&path, &editor, Some(&profile.snippets[0]), mtm);
    ensure(
        Profile::load(&path)? == profile,
        "Cancelled deletion changed profile",
    )?;
    add("Edit snippet", 2, vec![], None, "snippet-delete");
    add("Delete this snippet?", 1, vec![], None, "delete-confirm");
    ensure(
        edit_snippet(
            &path,
            &Editor::open(&path)?,
            Some(&profile.snippets[0]),
            mtm,
        )
        .contains("deleted"),
        "Native delete failed",
    )?;
    ensure(
        Profile::load(&path)?.snippets.is_empty(),
        "Confirmed deletion did not persist",
    )?;
    let plan = plan.borrow();
    if let Some(error) = &plan.failure {
        return Err(error.clone());
    }
    ensure(plan.steps.is_empty(), "Unconsumed modal driver steps")?;
    println!(
        "PASS: native language checkmarks, hints Save/Cancel/validation/locked/stale errors, multiline snippets and confirmed/cancelled deletion"
    );
    Ok(())
}
