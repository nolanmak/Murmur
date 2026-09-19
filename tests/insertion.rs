use murmur::insertion::{Target, allowed};
#[test]
fn messages_composer_uses_paste_even_when_ax_write_reports_success() {
    use murmur::insertion::native_paste_surface;
    for role in ["AXTextField", "AXTextArea"] {
        assert!(native_paste_surface("com.apple.MobileSMS", role, ""));
    }
    for role in ["AXButton", "AXGroup", "AXStaticText", "AXSecureTextField"] {
        assert!(!native_paste_surface("com.apple.MobileSMS", role, ""));
    }
    assert!(!native_paste_surface(
        "com.apple.MobileSMS",
        "AXTextField",
        "AXSecureTextField"
    ));
    assert!(!native_paste_surface(
        "com.apple.MobileSMS",
        "AXTextField",
        "AXPasswordField"
    ));
    assert!(native_paste_surface("com.apple.Notes", "AXTextField", ""));
    assert!(native_paste_surface("com.google.Chrome", "AXTextArea", ""));
    assert!(native_paste_surface("com.apple.Terminal", "AXTextArea", ""));
}
#[test]
fn any_app_text_input_uses_paste_without_a_bundle_allowlist() {
    use murmur::insertion::native_paste_surface;
    for bundle in [
        "net.whatsapp.WhatsApp",
        "com.tinyspeck.slackmacgap",
        "com.hnc.Discord",
        "com.example.unknown",
        "",
    ] {
        for role in ["AXTextField", "AXTextArea", "AXComboBox"] {
            assert!(native_paste_surface(bundle, role, ""), "{bundle} {role}");
            for subrole in ["AXSecureTextField", "AXPasswordField"] {
                assert!(!native_paste_surface(bundle, role, subrole));
            }
        }
        for role in [
            "AXButton",
            "AXGroup",
            "AXWebArea",
            "AXStaticText",
            "AXSecureTextField",
        ] {
            assert!(!native_paste_surface(bundle, role, ""));
        }
    }
}

#[test]
fn browser_editors_and_address_bars_use_native_paste() {
    use murmur::insertion::native_paste_surface as browser_surface;
    assert!(browser_surface("com.google.Chrome", "AXComboBox", ""));
    assert!(browser_surface("com.apple.Safari", "AXTextArea", ""));
    assert!(browser_surface("org.mozilla.firefox", "AXTextField", ""));
    assert!(!browser_surface("com.google.Chrome", "AXButton", ""));
    assert!(!browser_surface(
        "com.google.Chrome",
        "AXTextField",
        "AXSecureTextField"
    ));
    assert!(browser_surface("com.example.app", "AXComboBox", ""));
}
#[test]
fn terminal_surfaces_use_paste_instead_of_direct_ax_writes() {
    use murmur::insertion::terminal_surface;
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
    assert!(murmur::insertion::text_role("AXTextArea", ""));
    assert!(murmur::insertion::text_role("AXTextField", ""));
    assert!(!murmur::insertion::text_role(
        "AXTextField",
        "AXSecureTextField"
    ));
    assert!(!murmur::insertion::text_role("AXButton", ""));
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

#[test]
fn a_new_or_missing_window_observation_blocks_delivery() {
    use murmur::insertion::same_window;
    let equal = |before: &u64, after: &u64| before == after;
    assert!(same_window(Some(&7), Some(&7), equal));
    assert!(!same_window(Some(&7), Some(&8), equal));
    assert!(same_window::<u64>(None, None, equal));
    assert!(!same_window(Some(&7), None, equal));
    assert!(!same_window(None, Some(&7), equal));
}
