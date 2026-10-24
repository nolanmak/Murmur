use murmur::vocabulary::{Profile, Snippet};
fn profile() -> Profile {
    Profile {
        version: 1,
        recognition_hints: vec!["C++".into(), "Zoë & Sons".into()],
        snippets: vec![
            Snippet {
                name: "signature".into(),
                text: "Thanks,\nZoë".into(),
            },
            Snippet {
                name: "signature long".into(),
                text: "Thanks,\nZoë\nExample team".into(),
            },
            Snippet {
                name: "salut café".into(),
                text: "Bonjour café 👋".into(),
            },
        ],
    }
}
#[test]
fn deliberate_whole_utterance_cues_expand_without_touching_prose_or_code() {
    let p = profile();
    assert_eq!(p.expand("Insert snippet signature."), "Thanks,\nZoë");
    assert_eq!(
        p.expand("  insert snippet signature long!  "),
        "Thanks,\nZoë\nExample team"
    );
    assert_eq!(p.expand("INSERT SNIPPET SALUT CAFÉ?"), "Bonjour café 👋");
    for raw in [
        "please insert snippet signature",
        "insert snippet signatures",
        "insert snippet signature twice",
        "insert snippet signature. insert snippet signature.",
        "print(\"insert snippet signature\")",
        "insert snippet signature\nmore text",
        "don't insert snippet signature",
        "let name = signature;",
        "insert snippet unknown",
        "insert snippet signature...",
    ] {
        assert_eq!(p.expand(raw), raw);
    }
}
#[test]
fn invalid_versions_conflicts_controls_and_size_limits_are_refused() {
    let mut p = profile();
    p.version = 2;
    assert!(p.validate().is_err());
    let mut p = profile();
    p.snippets.push(Snippet {
        name: "SIGNATURE".into(),
        text: "conflict".into(),
    });
    assert!(p.validate().is_err());
    let mut p = profile();
    p.recognition_hints = vec!["a".repeat(501)];
    assert!(p.validate().is_err());
    let mut p = profile();
    p.recognition_hints = vec!["leak\nkeyterm=other".into()];
    assert!(p.validate().is_err());
    let mut p = profile();
    p.snippets[0].text = "x".repeat(16 * 1024 + 1);
    assert!(p.validate().is_err());
    let mut p = profile();
    p.snippets[0].text = "\u{1b}[2J".into();
    assert!(p.validate().is_err());
}
#[test]
fn failed_import_preserves_config_and_an_existing_utterance_keeps_its_snapshot() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("vocabulary.json");
    let original = profile();
    original.save(&path).unwrap();
    let snapshot = Profile::load(&path).unwrap();
    let before = std::fs::read(&path).unwrap();
    let mut invalid = original.clone();
    invalid.version = 999;
    assert!(invalid.save(&path).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), before);
    let mut updated = original;
    updated.snippets[0].text = "Updated".into();
    updated.save(&path).unwrap();
    assert_eq!(snapshot.expand("insert snippet signature"), "Thanks,\nZoë");
    assert_eq!(
        Profile::load(&path)
            .unwrap()
            .expand("insert snippet signature"),
        "Updated"
    );
    std::fs::write(&path, r#"{"version":1,"api_key":"fixture"}"#).unwrap();
    assert!(Profile::read(&path).is_err());
}

#[test]
fn byte_budget_and_import_size_are_bounded_at_the_boundary() {
    let mut p = Profile {
        recognition_hints: (0..4).map(|i| format!("{}{i}", "x".repeat(123))).collect(),
        ..Default::default()
    };
    assert!(p.validate().is_ok());
    p.recognition_hints.push("a".into());
    assert!(p.validate().is_err());
    p.recognition_hints = vec!["é".repeat(64)];
    assert!(p.validate().is_ok());
    p.recognition_hints.push("É".repeat(64));
    assert!(p.validate().is_err());
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("too-large.json");
    std::fs::write(&path, vec![b' '; murmur::vocabulary::MAX_PROFILE_BYTES + 1]).unwrap();
    assert!(Profile::read(&path).unwrap_err().contains("256 KiB"));
    assert!(Profile::read(&root.path().join("missing.json")).is_err());
    assert_eq!(
        Profile::load(&root.path().join("missing.json")).unwrap(),
        Profile::default()
    );
}

#[test]
fn cli_example_and_validation_use_only_the_requested_fixture_file() {
    let root = tempfile::tempdir().unwrap();
    let file = root.path().join("example.json");
    let executable = env!("CARGO_BIN_EXE_murmur");
    let made = std::process::Command::new(executable)
        .args(["vocabulary", "example"])
        .arg(&file)
        .output()
        .unwrap();
    assert!(
        made.status.success(),
        "{}",
        String::from_utf8_lossy(&made.stderr)
    );
    let profile = Profile::read(&file).unwrap();
    assert_eq!(profile.version, 1);
    assert_eq!(
        profile.expand("insert snippet sign off"),
        "Thanks,\nExample team"
    );
    let checked = std::process::Command::new(executable)
        .args(["vocabulary", "validate"])
        .arg(&file)
        .output()
        .unwrap();
    assert!(checked.status.success());
    std::fs::write(&file, r#"{"version":1,"api_key":"private-fixture-value"}"#).unwrap();
    let refused = std::process::Command::new(executable)
        .args(["vocabulary", "validate"])
        .arg(&file)
        .output()
        .unwrap();
    assert!(!refused.status.success());
    assert!(!String::from_utf8_lossy(&refused.stderr).contains("private-fixture-value"));
}
