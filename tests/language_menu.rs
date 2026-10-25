use murmur::vocabulary::ProfileFile as _;
use murmur::vocabulary::{Profile, Snippet};

#[test]
fn menu_choice_preserves_latest_vocabulary_and_the_active_utterance_snapshot() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("vocabulary.json");
    let initial = Profile {
        recognition_hints: vec!["Original hint".into()],
        snippets: vec![Snippet {
            name: "signature".into(),
            text: "Original body".into(),
        }],
        ..Default::default()
    };
    initial.save(&path).unwrap();
    let active = Profile::load(&path).unwrap();
    let mut externally_edited = initial;
    externally_edited.recognition_hints = vec!["New hint".into()];
    externally_edited.snippets[0].text = "New body".into();
    externally_edited.save(&path).unwrap();
    murmur::language::save_choice(&path, "fr").unwrap();
    let saved = Profile::load(&path).unwrap();
    assert_eq!(saved.language, "fr");
    assert_eq!(saved.recognition_hints, vec!["New hint"]);
    assert_eq!(saved.snippets[0].text, "New body");
    assert_eq!(active.language, "en");
    assert_eq!(active.snippets[0].text, "Original body");
}

#[test]
fn invalid_choice_or_profile_does_not_erase_existing_settings() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("vocabulary.json");
    Profile::default().save(&path).unwrap();
    let before = std::fs::read(&path).unwrap();
    assert!(murmur::language::save_choice(&path, "fr&translate=true").is_err());
    assert_eq!(std::fs::read(&path).unwrap(), before);
    let malformed = br#"{"version":999,"language":"en"}"#;
    std::fs::write(&path, malformed).unwrap();
    assert!(murmur::language::save_choice(&path, "fr").is_err());
    assert_eq!(std::fs::read(&path).unwrap(), malformed);
}

#[test]
fn menu_catalog_covers_common_local_languages_and_cloud_regional_choices_once() {
    let codes = murmur::language_catalog::codes();
    assert!(codes.contains(&"fr-CA"));
    assert!(codes.contains(&"yue"));
    assert!(codes.contains(&"haw"));
    assert_eq!(codes.iter().filter(|code| **code == "en").count(), 1);
    for code in murmur::language_catalog::COMMON {
        assert!(codes.contains(code));
    }
    assert_eq!(murmur::language_catalog::label("fr-CA"), "French (fr-CA)");
    assert_eq!(murmur::language_catalog::label("zz"), "zz");
    for code in codes {
        murmur::language::validate_tag(code).unwrap();
    }
}
