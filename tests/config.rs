use std::collections::HashMap;
use text_to_speech::config::resolve;
#[test]
fn finder_launch_finds_env_beside_app_even_with_unrelated_working_directory() {
    let dir = tempfile::tempdir().unwrap();
    let exe = dir
        .path()
        .join("Text-to-speech.app/Contents/MacOS/text-to-speech");
    let env = dir.path().join(".env");
    std::fs::write(&env, "DEEPGRAM_API_KEY=fixture\n").unwrap();
    assert_eq!(text_to_speech::config::bundle_env(&exe), Some(env));
    assert_eq!(
        text_to_speech::config::bundle_env(&dir.path().join("target/debug/text-to-speech")),
        None
    );
}
#[test]
fn process_key_wins_over_file_and_keychain_without_logging_secrets() {
    let mut vars = HashMap::new();
    vars.insert("DEEPGRAM_API_KEY".into(), "process-secret".into());
    let key = resolve(&vars, None, || panic!("keychain must not be read")).unwrap();
    assert_eq!(key.key.expose(), "process-secret");
    assert!(!format!("{key:?}").contains("process-secret"));
}
#[test]
fn dotenv_is_parsed_as_data_and_blank_key_uses_keychain() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join(".env");
    std::fs::write(&file, "DEEPGRAM_API_KEY='literal-$(not-executed)'\n").unwrap();
    let key = resolve(&HashMap::new(), Some(&file), || panic!()).unwrap();
    assert_eq!(key.key.expose(), "literal-$(not-executed)");
    std::fs::write(&file, "DEEPGRAM_API_KEY=\n").unwrap();
    assert_eq!(
        resolve(&HashMap::new(), Some(&file), || Ok(Some(
            "keychain-secret".into()
        )))
        .unwrap()
        .key
        .expose(),
        "keychain-secret"
    );
}
#[test]
fn malformed_or_missing_explicit_env_fails_without_exposing_contents() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join(".env");
    assert!(resolve(&HashMap::new(), Some(&file), || Ok(None)).is_err());
    std::fs::write(&file, "DEEPGRAM_API_KEY='secret-no-closing-quote").unwrap();
    let err = resolve(&HashMap::new(), Some(&file), || Ok(None)).unwrap_err();
    assert!(!err.contains("secret-no-closing-quote"));
}
