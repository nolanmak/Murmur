//! Native implementations must retain and compare the actual AX element too.
pub fn browser_surface(bundle: &str, role: &str, subrole: &str) -> bool {
    matches!(
        bundle,
        "com.google.Chrome"
            | "com.google.Chrome.beta"
            | "com.google.Chrome.canary"
            | "com.apple.Safari"
            | "com.apple.SafariTechnologyPreview"
            | "org.mozilla.firefox"
            | "com.microsoft.edgemac"
            | "com.brave.Browser"
            | "company.thebrowser.Browser"
    ) && text_role(role, subrole)
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
        _ => true,
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
