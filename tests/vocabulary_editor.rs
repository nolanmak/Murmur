use murmur::{
    vocabulary::{Profile, Snippet},
    vocabulary_editor::Editor,
};
fn snippet(name: &str, text: &str) -> Snippet {
    Snippet {
        name: name.into(),
        text: text.into(),
    }
}
#[test]
fn independent_dialogs_merge_language_hints_and_different_snippets() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("vocabulary.json");
    Profile {
        snippets: vec![snippet("signature", "Old"), snippet("address", "Somewhere")],
        ..Default::default()
    }
    .save(&path)
    .unwrap();
    let hints = Editor::open(&path).unwrap();
    let signature = Editor::open(&path).unwrap();
    let address = Editor::open(&path).unwrap();
    murmur::language::save_choice(&path, "fr").unwrap();
    hints.save_hints(&path, "  Café\n\nOpenAI  \n").unwrap();
    signature
        .save_snippet(
            &path,
            Some("signature"),
            snippet("sign off", "Hello\n\tWorld"),
        )
        .unwrap();
    address.delete_snippet(&path, "address").unwrap();
    let saved = Profile::load(&path).unwrap();
    assert_eq!(saved.language, "fr");
    assert_eq!(saved.recognition_hints, ["Café", "OpenAI"]);
    assert_eq!(saved.snippets, [snippet("sign off", "Hello\n\tWorld")]);
    assert_eq!(
        hints.baseline.language, "en",
        "active snapshots remain immutable"
    );
}
#[test]
fn stale_edits_and_deletes_preserve_newer_bytes() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("vocabulary.json");
    Profile {
        recognition_hints: vec!["Old".into()],
        snippets: vec![snippet("signature", "Old")],
        ..Default::default()
    }
    .save(&path)
    .unwrap();
    let editor = Editor::open(&path).unwrap();
    Profile {
        recognition_hints: vec!["New".into()],
        snippets: vec![snippet("signature", "New")],
        ..Default::default()
    }
    .save(&path)
    .unwrap();
    let before = std::fs::read(&path).unwrap();
    assert!(editor.save_hints(&path, "Mine").is_err());
    assert!(
        editor
            .save_snippet(&path, Some("signature"), snippet("signature", "Mine"))
            .is_err()
    );
    assert!(editor.delete_snippet(&path, "signature").is_err());
    assert_eq!(std::fs::read(&path).unwrap(), before);
}
#[test]
fn invalid_edits_and_corrupt_external_profiles_are_not_saved() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("vocabulary.json");
    let editor = Editor::open(&path).unwrap();
    editor
        .save_snippet(&path, None, snippet("signature", "Hello"))
        .unwrap();
    let before = std::fs::read(&path).unwrap();
    assert!(
        editor
            .save_snippet(&path, None, snippet("SIGNATURE", "Duplicate"))
            .is_err()
    );
    assert!(editor.save_hints(&path, "Term\nterm").is_err());
    assert_eq!(std::fs::read(&path).unwrap(), before);
    let invalid = br#"{"version":999}"#;
    std::fs::write(&path, invalid).unwrap();
    assert!(editor.save_hints(&path, "Term").is_err());
    assert_eq!(std::fs::read(&path).unwrap(), invalid);
}
#[test]
fn profile_lock_refuses_conflicting_writers_without_overwriting_or_waiting() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("vocabulary.json");
    Profile::default().save(&path).unwrap();
    let editor = Editor::open(&path).unwrap();
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(root.path().join("vocabulary.json.lock"))
        .unwrap();
    lock.lock().unwrap();
    let before = std::fs::read(&path).unwrap();
    let edited = path.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        let results = [
            editor.save_hints(&edited, "Term"),
            murmur::language::save_choice(&edited, "fr"),
            Profile {
                language: "es".into(),
                ..Default::default()
            }
            .save(&edited),
        ];
        let _ = tx.send(results);
        editor
    });
    let result = rx.recv_timeout(std::time::Duration::from_secs(1));
    // Release before joining on failure: a blocking-lock regression must fail
    // this bounded acknowledgment rather than hanging the entire test suite.
    drop(lock);
    let editor = worker.join().unwrap();
    assert!(
        result
            .expect("profile writers must refuse contention promptly")
            .iter()
            .all(Result::is_err)
    );
    assert_eq!(std::fs::read(&path).unwrap(), before);
    editor.save_hints(&path, "Term").unwrap();
}
