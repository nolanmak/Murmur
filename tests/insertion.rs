use text_to_speech::insertion::{Target, allowed};
#[test]
fn terminal_surfaces_use_paste_instead_of_direct_ax_writes() {
    use text_to_speech::insertion::terminal_surface;
    assert!(terminal_surface("com.apple.Terminal", "AXTextArea", ""));
    assert!(terminal_surface("com.mitchellh.ghostty", "AXGroup", ""));
    assert!(terminal_surface(
        "com.googlecode.iterm2",
        "AXScrollArea",
        ""
    ));
    assert!(!terminal_surface("com.example.app", "AXGroup", ""));
    assert!(!terminal_surface("com.apple.Terminal", "AXButton", ""));
    assert!(!terminal_surface(
        "com.apple.Terminal",
        "AXTextField",
        "AXSecureTextField"
    ));
}
#[test]
fn web_text_fields_do_not_need_direct_ax_write_support() {
    assert!(text_to_speech::insertion::text_role("AXTextArea", ""));
    assert!(text_to_speech::insertion::text_role("AXTextField", ""));
    assert!(!text_to_speech::insertion::text_role(
        "AXTextField",
        "AXSecureTextField"
    ));
    assert!(!text_to_speech::insertion::text_role("AXButton", ""));
}
fn target() -> Target {
    Target {
        pid: 42,
        element: 7,
        secure: false,
        editable: true,
    }
}
#[test]
fn focus_drift_passwords_and_noneditors_block_insertion() {
    let initial = target();
    assert!(allowed(&initial, &initial, "Hello 👋"));
    let mut changed = target();
    changed.pid = 43;
    assert!(!allowed(&initial, &changed, "hello"));
    let mut changed = target();
    changed.element = 8;
    assert!(!allowed(&initial, &changed, "hello"));
    let mut secure = target();
    secure.secure = true;
    assert!(!allowed(&secure, &secure, "hello"));
    let mut unsupported = target();
    unsupported.editable = false;
    assert!(!allowed(&unsupported, &unsupported, "hello"));
    assert!(!allowed(&initial, &initial, ""));
}
#[test]
fn linebreaks_and_control_characters_never_become_automatic_submit_keys() {
    let t = target();
    assert!(!allowed(&t, &t, "echo hi\n"));
    assert!(!allowed(&t, &t, "hi\r"));
    assert!(!allowed(&t, &t, "hi\t"));
}
