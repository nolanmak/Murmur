//! Native implementations must retain and compare the actual AX element too.
pub fn native_paste_surface(bundle: &str, role: &str, subrole: &str) -> bool {
    // A successful AXSelectedText write does not prove the editor consumed it.
    // Use the app's normal paste handling for every recognized text input.
    text_role(role, subrole) || terminal_surface(bundle, role, subrole)
}

pub fn terminal_surface(bundle: &str, role: &str, subrole: &str) -> bool {
    matches!(
        bundle,
        "com.apple.Terminal" | "com.googlecode.iterm2" | "com.mitchellh.ghostty"
    ) && matches!(
        role,
        "AXTextArea" | "AXTextField" | "AXGroup" | "AXScrollArea"
    ) && !subrole.contains("Secure")
        && !subrole.contains("Password")
}
pub fn text_role(role: &str, subrole: &str) -> bool {
    matches!(role, "AXTextField" | "AXTextArea" | "AXComboBox")
        && !subrole.contains("Secure")
        && !subrole.contains("Password")
}

/// Compare two observations of the focused window.
pub fn same_window<T>(
    before: Option<&T>,
    after: Option<&T>,
    equal: impl FnOnce(&T, &T) -> bool,
) -> bool {
    match (before, after) {
        (Some(before), Some(after)) => equal(before, after),
        (None, None) => true,
        _ => false,
    }
}
#[derive(Clone, Debug)]
pub struct Target {
    pub pid: i32,
    pub element: u64,
    pub secure: bool,
    pub editable: bool,
}
pub fn allowed(a: &Target, b: &Target, text: &str) -> bool {
    a.pid > 0
        && a.pid == b.pid
        && a.element == b.element
        && !a.secure
        && !b.secure
        && a.editable
        && b.editable
        && !text.trim().is_empty()
        && !text.chars().any(char::is_control)
}

/// How the focused element looks to Accessibility right now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Probe {
    Editable,
    Secure,
    NotEditable,
}
pub const FOCUS_STEP: std::time::Duration = std::time::Duration::from_millis(50);
pub const FOCUS_WAIT: std::time::Duration = std::time::Duration::from_millis(750);
/// Re-probes focus while it is missing or not yet editable. Browsers such as Chrome
/// build their accessibility tree asynchronously after it is enabled, so the first
/// read can be empty. Returns the last observation once ready or out of budget.
pub fn settle_focus<T>(
    probe: impl FnMut() -> Option<T>,
    classify: impl Fn(&T) -> Probe,
    sleep: impl FnMut(std::time::Duration),
) -> Option<T> {
    let started = std::time::Instant::now();
    settle_focus_with_clock(probe, classify, sleep, || started.elapsed())
}

/// The injected monotonic clock includes time spent reading accessibility state.
pub fn settle_focus_with_clock<T>(
    mut probe: impl FnMut() -> Option<T>,
    classify: impl Fn(&T) -> Probe,
    mut sleep: impl FnMut(std::time::Duration),
    mut elapsed: impl FnMut() -> std::time::Duration,
) -> Option<T> {
    let started = elapsed();
    loop {
        let seen = probe();
        let kind = seen.as_ref().map(&classify);
        let waited = elapsed().saturating_sub(started);
        if kind == Some(Probe::Secure) {
            return seen;
        }
        if waited > FOCUS_WAIT {
            return None;
        }
        if kind == Some(Probe::Editable) || waited >= FOCUS_WAIT {
            return seen;
        }
        sleep(FOCUS_STEP.min(FOCUS_WAIT - waited));
    }
}

/// What to do with the focus observed when dictation starts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Preflight {
    /// Deliver into this focused field.
    Use,
    /// Remote review records without authorizing local insertion.
    ReviewOnly,
    Refuse(crate::local_delivery::Failure),
}
pub fn preflight(seen: Option<Probe>, remote: bool) -> Preflight {
    use crate::local_delivery::Failure;
    match seen {
        Some(Probe::Secure) => Preflight::Refuse(Failure::SecureTarget),
        Some(Probe::Editable) if !remote => Preflight::Use,
        Some(Probe::NotEditable) | None if !remote => Preflight::Refuse(Failure::NoFocusedField),
        _ => Preflight::ReviewOnly,
    }
}
const CHROMIUM_BROWSERS: [&str; 8] = [
    "com.google.Chrome",
    "com.google.Chrome.beta",
    "com.google.Chrome.canary",
    "org.chromium.Chromium",
    "com.brave.Browser",
    "com.microsoft.edgemac",
    "company.thebrowser.Browser",
    "com.vivaldi.Vivaldi",
];
/// Attributes set on the frontmost app ahead of dictation so its accessibility tree
/// is built before Control is held. `AXManualAccessibility` is Electron's switch and
/// is ignored elsewhere; `AXEnhancedUserInterface` can affect window animation, so it
/// is limited to Chromium browsers that need it.
pub fn prewarm_attributes(bundle: &str) -> &'static [&'static str] {
    if CHROMIUM_BROWSERS.contains(&bundle) {
        &["AXManualAccessibility", "AXEnhancedUserInterface"]
    } else {
        &["AXManualAccessibility"]
    }
}
