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

mod settle {
    use murmur::insertion::{FOCUS_STEP, FOCUS_WAIT, Probe, settle_focus_with_clock};
    use std::cell::Cell;
    use std::time::Duration;

    /// Runs `settle_focus` over scripted probes with a fake sleep; returns (result, probes, slept).
    fn run(script: &[Option<Probe>]) -> (Option<Probe>, usize, Duration) {
        let calls = Cell::new(0);
        let slept = Cell::new(Duration::ZERO);
        let result = settle_focus_with_clock(
            || {
                let i = calls.get();
                calls.set(i + 1);
                script[i.min(script.len() - 1)]
            },
            |probe| *probe,
            |step| slept.set(slept.get() + step),
            || slept.get(),
        );
        (result, calls.get(), slept.get())
    }

    #[test]
    fn editable_focus_is_used_without_waiting() {
        assert_eq!(
            run(&[Some(Probe::Editable)]),
            (Some(Probe::Editable), 1, Duration::ZERO)
        );
    }

    #[test]
    fn browser_tree_that_appears_later_is_found() {
        // Chrome reports nothing, then its web area, then the composer.
        let (result, probes, slept) =
            run(&[None, None, Some(Probe::NotEditable), Some(Probe::Editable)]);
        assert_eq!(result, Some(Probe::Editable));
        assert_eq!(probes, 4);
        assert_eq!(slept, FOCUS_STEP * 3);
    }

    #[test]
    fn secure_fields_stop_immediately() {
        assert_eq!(
            run(&[Some(Probe::Secure)]),
            (Some(Probe::Secure), 1, Duration::ZERO)
        );
    }

    #[test]
    fn gives_up_at_the_budget_with_the_last_observation() {
        let (result, probes, slept) = run(&[None]);
        assert_eq!(result, None);
        assert_eq!(slept, FOCUS_WAIT);
        assert_eq!(
            probes as u32,
            FOCUS_WAIT.as_millis() as u32 / FOCUS_STEP.as_millis() as u32 + 1
        );
        let (result, _, slept) = run(&[Some(Probe::NotEditable)]);
        assert_eq!(result, Some(Probe::NotEditable));
        assert_eq!(slept, FOCUS_WAIT);
    }

    #[test]
    fn budget_matches_the_issue() {
        assert_eq!(FOCUS_STEP, Duration::from_millis(50));
        assert_eq!(FOCUS_WAIT, Duration::from_millis(750));
    }
}

mod start {
    use murmur::insertion::{Preflight, Probe, preflight, prewarm_attributes};
    use murmur::local_delivery::Failure;

    #[test]
    fn remote_review_does_not_require_a_local_target() {
        assert_eq!(preflight(None, true), Preflight::ReviewOnly);
    }

    #[test]
    fn known_targets_keep_their_existing_decisions() {
        assert_eq!(preflight(Some(Probe::Editable), false), Preflight::Use);
        assert_eq!(
            preflight(Some(Probe::Editable), true),
            Preflight::ReviewOnly
        );
        for remote in [false, true] {
            assert_eq!(
                preflight(Some(Probe::Secure), remote),
                Preflight::Refuse(Failure::SecureTarget)
            );
        }
        assert_eq!(
            preflight(Some(Probe::NotEditable), false),
            Preflight::Refuse(Failure::NoFocusedField)
        );
        assert_eq!(
            preflight(Some(Probe::NotEditable), true),
            Preflight::ReviewOnly
        );
    }

    #[test]
    fn prewarm_turns_on_electron_accessibility_everywhere_and_browser_mode_only_for_chromium() {
        for bundle in [
            "com.hnc.Discord",
            "com.tinyspeck.slackmacgap",
            "com.apple.Notes",
            "",
        ] {
            assert_eq!(
                prewarm_attributes(bundle),
                ["AXManualAccessibility"],
                "{bundle}"
            );
        }
        for bundle in [
            "com.google.Chrome",
            "com.brave.Browser",
            "com.microsoft.edgemac",
            "company.thebrowser.Browser",
        ] {
            assert_eq!(
                prewarm_attributes(bundle),
                ["AXManualAccessibility", "AXEnhancedUserInterface"],
                "{bundle}"
            );
        }
    }
}
#[test]
fn missing_local_target_is_refused_before_recording() {
    use murmur::{
        insertion::{Preflight, preflight},
        local_delivery::Failure,
    };
    assert_eq!(
        preflight(None, false),
        Preflight::Refuse(Failure::NoFocusedField)
    );
    assert!(
        !Failure::NoFocusedField
            .message()
            .contains("Transcript ready")
    );
}

#[test]
fn slow_accessibility_probes_consume_the_focus_budget() {
    use murmur::insertion::{Probe, settle_focus_with_clock};
    use std::{cell::Cell, time::Duration};
    let elapsed = Cell::new(Duration::ZERO);
    let calls = Cell::new(0);
    let slept = Cell::new(Duration::ZERO);
    let result = settle_focus_with_clock(
        || {
            let call = calls.get();
            calls.set(call + 1);
            elapsed.set(elapsed.get() + Duration::from_millis(if call == 0 { 600 } else { 200 }));
            (call > 0).then_some(Probe::Editable)
        },
        |probe| *probe,
        |step| {
            slept.set(slept.get() + step);
            elapsed.set(elapsed.get() + step);
        },
        || elapsed.get(),
    );
    assert_eq!(
        result, None,
        "a field first exposed after the deadline cannot authorize capture"
    );
    assert_eq!(calls.get(), 2);
    assert_eq!(slept.get(), Duration::from_millis(50));
}
