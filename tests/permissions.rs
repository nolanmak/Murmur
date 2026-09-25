use murmur::indicator::{Kind, NOTICE_CHARS, classify, shorten};
use murmur::permissions::*;

const ALL_GRANTED: Snapshot = Snapshot {
    microphone: Access::Granted,
    accessibility: true,
    input_monitoring: true,
    ad_hoc: false,
};

#[test]
fn nothing_is_missing_when_every_grant_is_present() {
    assert!(ALL_GRANTED.missing().is_empty());
    assert_eq!(ALL_GRANTED.warning(), None);
    assert_eq!(ALL_GRANTED.gate(Route::Local), Ok(()));
    assert_eq!(ALL_GRANTED.gate(Route::RemoteReview), Ok(()));
}

#[test]
fn missing_grants_are_listed_microphone_then_accessibility_then_input_monitoring() {
    let none = Snapshot {
        microphone: Access::Denied,
        accessibility: false,
        input_monitoring: false,
        ad_hoc: false,
    };
    assert_eq!(
        none.missing(),
        [
            Grant::Microphone,
            Grant::Accessibility,
            Grant::InputMonitoring
        ]
    );
    let undetermined = Snapshot {
        microphone: Access::Undetermined,
        ..ALL_GRANTED
    };
    assert_eq!(undetermined.missing(), [Grant::Microphone]);
}

#[test]
fn local_dictation_is_blocked_before_recording_without_accessibility() {
    // Accessibility is what inserts text; recording without it can only end in
    // "Transcript ready · choose Copy Last Transcript".
    let snapshot = Snapshot {
        accessibility: false,
        ..ALL_GRANTED
    };
    assert_eq!(snapshot.gate(Route::Local), Err(Grant::Accessibility));
    // Remote review only copies to the clipboard after an explicit review.
    assert_eq!(snapshot.gate(Route::RemoteReview), Ok(()));
}

#[test]
fn every_route_is_blocked_without_microphone_and_microphone_is_reported_first() {
    for microphone in [Access::Denied, Access::Undetermined] {
        let snapshot = Snapshot {
            microphone,
            accessibility: false,
            ..ALL_GRANTED
        };
        assert_eq!(snapshot.gate(Route::Local), Err(Grant::Microphone));
        assert_eq!(snapshot.gate(Route::RemoteReview), Err(Grant::Microphone));
    }
}

#[test]
fn input_monitoring_does_not_block_menu_started_dictation_but_is_warned() {
    let snapshot = Snapshot {
        input_monitoring: false,
        ..ALL_GRANTED
    };
    assert_eq!(snapshot.gate(Route::Local), Ok(()));
    let warning = snapshot.warning().unwrap();
    assert!(warning.contains("Input Monitoring"), "{warning}");
}

#[test]
fn warnings_name_the_missing_grant_and_what_breaks() {
    for (snapshot, name, effect) in [
        (
            Snapshot {
                microphone: Access::Denied,
                ..ALL_GRANTED
            },
            "Microphone",
            "can't hear",
        ),
        (
            Snapshot {
                accessibility: false,
                ..ALL_GRANTED
            },
            "Accessibility",
            "can't paste",
        ),
        (
            Snapshot {
                input_monitoring: false,
                ..ALL_GRANTED
            },
            "Input Monitoring",
            "Control",
        ),
    ] {
        let warning = snapshot.warning().unwrap();
        assert!(warning.contains(name), "{warning}");
        assert!(warning.contains(effect), "{warning}");
        assert!(warning.contains("Set up permissions"), "{warning}");
    }
}

#[test]
fn several_missing_grants_are_all_named() {
    let snapshot = Snapshot {
        microphone: Access::Denied,
        accessibility: false,
        input_monitoring: false,
        ad_hoc: false,
    };
    let warning = snapshot.warning().unwrap();
    for name in ["Microphone", "Accessibility", "Input Monitoring"] {
        assert!(warning.contains(name), "{warning}");
    }
}

#[test]
fn ad_hoc_builds_explain_that_old_grants_do_not_carry_over() {
    let snapshot = Snapshot {
        accessibility: false,
        ad_hoc: true,
        ..ALL_GRANTED
    };
    let warning = snapshot.warning().unwrap();
    assert!(warning.contains("remove Murmur"), "{warning}");
    assert!(warning.contains("add it again"), "{warning}");
    let blocked = blocked_message(Grant::Accessibility, &snapshot);
    assert!(blocked.contains("remove Murmur"), "{blocked}");
    // A fully granted ad-hoc build has nothing to warn about.
    let fine = Snapshot {
        ad_hoc: true,
        ..ALL_GRANTED
    };
    assert_eq!(fine.warning(), None);
}

#[test]
fn blocked_messages_fit_the_pill_with_the_grant_name_intact() {
    for grant in [
        Grant::Microphone,
        Grant::Accessibility,
        Grant::InputMonitoring,
    ] {
        for ad_hoc in [false, true] {
            let snapshot = Snapshot {
                ad_hoc,
                ..ALL_GRANTED
            };
            let message = blocked_message(grant, &snapshot);
            assert_eq!(classify(&message), Kind::Notice);
            let pill = shorten(&message);
            assert!(pill.chars().count() <= NOTICE_CHARS, "{pill}");
            assert!(pill.contains(grant.name()), "{pill}");
        }
    }
}

#[test]
fn each_grant_opens_its_own_settings_pane() {
    let base = "x-apple.systempreferences:com.apple.preference.security?";
    assert_eq!(
        Grant::Microphone.settings_url(),
        format!("{base}Privacy_Microphone")
    );
    assert_eq!(
        Grant::Accessibility.settings_url(),
        format!("{base}Privacy_Accessibility")
    );
    assert_eq!(
        Grant::InputMonitoring.settings_url(),
        format!("{base}Privacy_ListenEvent")
    );
}

#[test]
fn undetermined_microphone_asks_the_system_prompt_and_denied_opens_settings() {
    let undetermined = Snapshot {
        microphone: Access::Undetermined,
        ..ALL_GRANTED
    };
    assert_eq!(
        request(Grant::Microphone, &undetermined),
        Request::MicrophonePrompt
    );
    let denied = Snapshot {
        microphone: Access::Denied,
        ..ALL_GRANTED
    };
    assert_eq!(
        request(Grant::Microphone, &denied),
        Request::OpenSettings(Grant::Microphone)
    );
    assert_eq!(
        request(Grant::Accessibility, &denied),
        Request::AccessibilityPrompt
    );
    assert_eq!(
        request(Grant::InputMonitoring, &denied),
        Request::InputMonitoringPrompt
    );
}

#[test]
fn onboarding_asks_one_grant_at_a_time_and_never_repeats_itself() {
    let mut onboarding = Onboarding::default();
    let mut snapshot = Snapshot {
        microphone: Access::Undetermined,
        accessibility: false,
        input_monitoring: false,
        ad_hoc: false,
    };
    assert_eq!(onboarding.next(&snapshot), Some(Grant::Microphone));
    // While the microphone prompt is pending, nothing else is stacked on top.
    assert_eq!(onboarding.next(&snapshot), None);
    assert_eq!(onboarding.next(&snapshot), None);
    snapshot.microphone = Access::Granted;
    assert_eq!(onboarding.next(&snapshot), Some(Grant::Accessibility));
    assert_eq!(onboarding.next(&snapshot), None);
    snapshot.accessibility = true;
    assert_eq!(onboarding.next(&snapshot), Some(Grant::InputMonitoring));
    snapshot.input_monitoring = true;
    assert_eq!(onboarding.next(&snapshot), None);
    // Revoking later does not re-prompt within the same launch; the warning covers it.
    snapshot.accessibility = false;
    assert_eq!(onboarding.next(&snapshot), None);
}

#[test]
fn a_declined_prompt_does_not_stall_later_grants() {
    let mut onboarding = Onboarding::default();
    let snapshot = Snapshot {
        microphone: Access::Denied,
        accessibility: false,
        input_monitoring: true,
        ad_hoc: false,
    };
    assert_eq!(onboarding.next(&snapshot), Some(Grant::Microphone));
    // The user answered the microphone step (denied): move on rather than wait forever.
    onboarding.answered();
    assert_eq!(onboarding.next(&snapshot), Some(Grant::Accessibility));
    onboarding.answered();
    assert_eq!(onboarding.next(&snapshot), None);
}

#[test]
fn newly_granted_input_monitoring_is_detected_so_the_listener_can_restart() {
    let before = Snapshot {
        input_monitoring: false,
        ..ALL_GRANTED
    };
    assert_eq!(
        newly_granted(&before, &ALL_GRANTED),
        [Grant::InputMonitoring]
    );
    assert!(newly_granted(&ALL_GRANTED, &ALL_GRANTED).is_empty());
    assert!(newly_granted(&ALL_GRANTED, &before).is_empty());
}

#[test]
fn code_signing_flags_identify_ad_hoc_builds() {
    const ADHOC: u32 = 0x0002;
    const LINKER_SIGNED: u32 = 0x2_0000;
    assert!(is_ad_hoc(ADHOC));
    assert!(is_ad_hoc(ADHOC | LINKER_SIGNED));
    assert!(!is_ad_hoc(0));
    assert!(!is_ad_hoc(0x1_0000));
}

#[test]
fn startup_report_lists_every_grant_and_signature_kind() {
    let snapshot = Snapshot {
        microphone: Access::Undetermined,
        accessibility: false,
        ad_hoc: true,
        ..ALL_GRANTED
    };
    assert_eq!(
        snapshot.report(true),
        "permissions microphone=undetermined accessibility=false input_monitoring=true keyboard_listener=true signature=adhoc"
    );
    assert!(
        ALL_GRANTED
            .report(false)
            .ends_with("keyboard_listener=false signature=certificate")
    );
}

#[test]
fn input_monitoring_warning_says_murmur_must_be_reopened() {
    // macOS applies an Input Monitoring grant only to newly launched processes.
    let snapshot = Snapshot {
        input_monitoring: false,
        ..ALL_GRANTED
    };
    let warning = snapshot.warning().unwrap();
    assert!(warning.contains("Reopen Murmur"), "{warning}");
    let mixed = Snapshot {
        accessibility: false,
        input_monitoring: false,
        ..ALL_GRANTED
    };
    assert!(mixed.warning().unwrap().contains("Reopen Murmur"));
    let without = Snapshot {
        accessibility: false,
        ..ALL_GRANTED
    };
    assert!(!without.warning().unwrap().contains("Reopen Murmur"));
}

#[test]
fn blocked_message_matches_what_actually_opens() {
    let undetermined = Snapshot {
        microphone: Access::Undetermined,
        ..ALL_GRANTED
    };
    let prompt = blocked_message(Grant::Microphone, &undetermined);
    assert!(prompt.contains("prompt"), "{prompt}");
    assert!(!prompt.contains("System Settings"), "{prompt}");
    let denied = Snapshot {
        microphone: Access::Denied,
        ..ALL_GRANTED
    };
    let settings = blocked_message(Grant::Microphone, &denied);
    assert!(settings.contains("Opening System Settings"), "{settings}");
}

#[test]
fn reopen_targets_the_enclosing_app_bundle_only() {
    use std::path::Path;
    assert_eq!(
        app_bundle(Path::new("/Apps/Murmur.app/Contents/MacOS/murmur")),
        Some(Path::new("/Apps/Murmur.app").to_path_buf())
    );
    assert_eq!(app_bundle(Path::new("/repo/target/debug/murmur")), None);
}
