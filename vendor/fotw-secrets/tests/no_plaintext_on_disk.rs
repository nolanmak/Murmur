//! **KEY-01 acceptance test.** Write every known test key, then byte-scan
//! every file the app could have written and require zero hits.
//!
//! > CI test: write every known test key, close the DB, then byte-scan
//! > `db.sqlite3`, `-wal`, `-shm` and every file under `<root>` for each key
//! > string — zero hits required.
//! > — docs/REQUIREMENTS.md 10
//!
//! # Why this test is built the way it is
//!
//! A byte-scan acceptance test has an obvious failure mode: **if nothing ever
//! writes a file, the scan finds nothing and the test proves nothing.** It
//! passes just as green against an implementation that does nothing at all, so
//! by default it measures the test harness rather than the system. Three
//! separate guards make that impossible here:
//!
//! 1. **The scanner is proved to work, in the same run, by the same code
//!    path.** [`positive_control`] plants each key verbatim in a directory
//!    outside the app root and asserts the scanner *finds* every one. A
//!    scanner that always returns "no hits" fails there before it can pass
//!    anywhere else.
//! 2. **Secrets are deliberately pushed at the disk.** The test does not
//!    politely avoid writing keys — it writes log lines that embed the raw
//!    material, `Debug`- and `Display`-formats secrets into them, and puts a
//!    credential header in the log, all through the redacting path. Then it
//!    asserts the file is non-empty, retained its non-secret context, and
//!    carries redaction markers. So we know bytes were written, we know they
//!    were the *right* bytes, and only then does the absence of key material
//!    mean anything.
//! 3. **The scan is required to have covered something.** It asserts a minimum
//!    file count, a non-zero byte total, and the presence of each specific
//!    file it expects to have visited — so a scan that silently skipped the
//!    directory (a `read_dir` error swallowed, a bad root path) fails loudly
//!    instead of reporting a clean sweep of nothing.
//!
//! Guard 1 is the important one. It converts "the scan found no keys" from an
//! unfalsifiable claim into a comparison against a known-positive result
//! produced by the same function moments earlier.

use std::collections::BTreeSet;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use fotw_secrets::{
    CredentialRecord, InMemoryKeyStore, KeyStore, OsKeyStore, RedactingWriter, Redactor, SecretKey,
    SecretString, os_tests_enabled,
};

// ---------------------------------------------------------------- fixtures

/// Distinctive material per key. Long, unique, and containing nothing that
/// occurs naturally in JSON, a log line, or a SQLite header — so a hit is a
/// real hit and not a coincidence.
fn material_for(key: SecretKey) -> String {
    format!(
        "fotw-acceptance-{}-K3YM4T3R14L-8f3a2b1c9d0e4f5a6b7c",
        key.account().replace(':', "-")
    )
}

/// Every key the app stores, with its test material.
///
/// Driven off [`SecretKey::ALL`] rather than a hand-written list: a new
/// provider added to the enum is covered by this test automatically, instead
/// of silently narrowing it.
fn test_keys() -> Vec<(SecretKey, String)> {
    SecretKey::ALL
        .into_iter()
        .map(|key| (key, material_for(key)))
        .collect()
}

// ----------------------------------------------------------------- scanner

/// What a scan looked at and what it found.
#[derive(Debug, Default)]
struct ScanReport {
    files: Vec<PathBuf>,
    bytes: u64,
    hits: Vec<Hit>,
}

/// One occurrence of key material on disk.
#[derive(Debug)]
struct Hit {
    path: PathBuf,
    account: String,
}

impl ScanReport {
    fn describe_hits(&self) -> String {
        self.hits
            .iter()
            .map(|hit| format!("  {} in {}", hit.account, hit.path.display()))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// Byte-scan every regular file under `root` for every supplied needle.
///
/// Deliberately byte-oriented, not text-oriented: it must catch a key inside a
/// SQLite page, a compressed frame boundary, or a file that is not valid
/// UTF-8. Reading as text and hoping would miss exactly the cases that matter.
fn scan(root: &Path, needles: &[(SecretKey, String)]) -> ScanReport {
    let mut report = ScanReport::default();
    scan_dir(root, needles, &mut report);
    report.files.sort();
    report
}

fn scan_dir(dir: &Path, needles: &[(SecretKey, String)], report: &mut ScanReport) {
    // Any failure to read is a failure of the test, not a clean result. A
    // swallowed error here is precisely how this test would go vacuous.
    let entries =
        fs::read_dir(dir).unwrap_or_else(|err| panic!("cannot scan {}: {err}", dir.display()));

    for entry in entries {
        let entry = entry.expect("cannot read directory entry");
        let path = entry.path();
        let file_type = entry.file_type().expect("cannot stat entry");

        if file_type.is_dir() {
            scan_dir(&path, needles, report);
            continue;
        }
        if !file_type.is_file() {
            // Symlinks are not followed: the target is either inside the root
            // (and scanned on its own) or outside it (and not ours).
            continue;
        }

        let contents =
            fs::read(&path).unwrap_or_else(|err| panic!("cannot read {}: {err}", path.display()));
        report.bytes += contents.len() as u64;
        report.files.push(path.clone());

        for (key, material) in needles {
            if contains(&contents, material.as_bytes()) {
                report.hits.push(Hit {
                    path: path.clone(),
                    account: key.account(),
                });
            }
        }
    }
}

/// Naive substring search over bytes.
fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() || needle.len() > haystack.len() {
        return false;
    }
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

// -------------------------------------------------------- guard 1: control

/// **Proves the scanner is not a no-op.**
///
/// Plants every key verbatim in a directory of its own and requires the
/// scanner to find all of them. If this fails, every "zero hits" result in
/// this file is meaningless and the suite says so here rather than passing
/// green on a broken scan.
fn positive_control(dir: &Path, keys: &[(SecretKey, String)]) {
    fs::create_dir_all(dir).unwrap();
    for (key, material) in keys {
        let path = dir.join(format!("{}.leak", key.account().replace(':', "-")));
        fs::write(&path, format!("key = {material}\n")).unwrap();
    }
    // ...and one file with no key at all, so "hits == keys.len()" also proves
    // the scanner is not simply flagging every file it opens.
    fs::write(dir.join("innocent.txt"), b"nothing to see here\n").unwrap();

    let report = scan(dir, keys);

    assert_eq!(
        report.hits.len(),
        keys.len(),
        "the scanner failed to find planted keys, so it cannot prove their absence \
         anywhere else.\nfound:\n{}",
        report.describe_hits()
    );
    assert_eq!(report.files.len(), keys.len() + 1, "scanner skipped files");
    assert!(report.bytes > 0);
}

// --------------------------------------------------------- the app's writes

/// Everything the application could plausibly put on disk while handling keys.
///
/// Each of these is a real leak path: the credentials index is the row the DB
/// holds, the log is where a `{:?}` ends up, and the header line is what an
/// HTTP wrapper traces. They are exercised *with real key material in hand* —
/// this function is trying to write the keys to disk, through the paths that
/// are supposed to stop it.
fn exercise_every_write_path(root: &Path, store: &dyn KeyStore, keys: &[(SecretKey, String)]) {
    let config = root.join("config");
    let data = root.join("data");
    let logs = root.join("logs");
    for dir in [&config, &data, &logs] {
        fs::create_dir_all(dir).unwrap();
    }

    // The redactor is the control under test. Register every live key, exactly
    // as the daemon would on startup after reading the keychain.
    let redactor = Arc::new(Redactor::new());
    for (_, material) in keys {
        redactor
            .register(&SecretString::new(material.clone()))
            .expect("test keys are long enough to register");
    }

    // 1. Store every key, and build the credentials index rows.
    let mut records = Vec::new();
    for (index, (key, material)) in keys.iter().enumerate() {
        let secret = SecretString::new(material.clone());
        store.set(*key, &secret).expect("store rejected a key");

        records.push(CredentialRecord::describe(
            format!("01JD8QK00000000000000000{index:02}"),
            *key,
            &secret,
            Some(format!("{} test key", key.account())),
            1_754_000_000_000 + index as i64,
        ));
    }

    // 2. Persist the credentials index. This stands in for the SQLite table:
    //    the file names are the ones docs/REQUIREMENTS.md 10 calls out, and
    //    the payload is the real serialised rows, so if a row could carry key
    //    material the scan would find it here.
    let index_json = serde_json::to_string_pretty(&records).unwrap();
    fs::write(data.join("db.sqlite3"), &index_json).unwrap();
    fs::write(data.join("db.sqlite3-wal"), &index_json).unwrap();
    fs::write(data.join("db.sqlite3-shm"), &index_json).unwrap();
    fs::write(config.join("credentials.json"), &index_json).unwrap();

    // 3. Write a log through the redacting sink, deliberately embedding key
    //    material in the ways it actually leaks.
    let log_file = fs::File::create(logs.join("fotwd.log")).unwrap();
    {
        let mut log = RedactingWriter::new(log_file, Arc::clone(&redactor));

        writeln!(
            log,
            "session-start build=0.1.0 provider-count={}",
            keys.len()
        )
        .unwrap();

        for (key, material) in keys {
            let secret = SecretString::new(material.clone());

            // (a) the raw key, straight into a log line
            writeln!(log, "configured {} with key {material}", key.account()).unwrap();

            // (b) the credential header an HTTP tracer would emit
            writeln!(log, "GET /v1/projects Authorization: Token {material}").unwrap();

            // (c) a key read back out of the store and formatted both ways
            let loaded = store.get(*key).expect("stored key vanished");
            writeln!(log, "loaded {key} debug={loaded:?} display={loaded}").unwrap();

            // (d) a struct that happens to contain a secret, `Debug`-printed
            #[derive(Debug)]
            #[allow(dead_code)]
            struct ProviderConfig<'a> {
                account: &'a str,
                key: SecretString,
            }
            let cfg = ProviderConfig {
                account: "apikey:deepgram",
                key: secret,
            };
            writeln!(log, "config {cfg:?}").unwrap();

            // (e) a JSON payload with the key interpolated, as a request
            //     body tracer would produce
            writeln!(log, r#"body {{"api_key":"{material}"}}"#).unwrap();
        }

        writeln!(log, "session-end clean=true").unwrap();
        log.flush().unwrap();
    }

    // 4. A plain, non-secret artifact, so the scan has something it must not
    //    flag and the log is not the only file with content.
    fs::write(
        config.join("settings.json"),
        br#"{"theme":"dark","retain_audio_days":30}"#,
    )
    .unwrap();
}

/// Every path under `root`, for before/after comparison.
fn file_set(root: &Path) -> BTreeSet<PathBuf> {
    scan(root, &[]).files.into_iter().collect()
}

// ------------------------------------------------------------- assertions

/// Guard 2: prove the redacting path actually ran and actually wrote.
fn assert_the_log_was_written_and_redacted(root: &Path, keys: &[(SecretKey, String)]) {
    let log = fs::read_to_string(root.join("logs").join("fotwd.log")).unwrap();

    assert!(
        !log.is_empty(),
        "the log is empty; nothing was written to scan"
    );
    assert!(
        log.contains("session-start"),
        "the log lost its non-secret content, so redaction is just deleting everything"
    );
    assert!(
        log.contains("session-end clean=true"),
        "the log was truncated"
    );
    assert!(
        log.contains("[REDACTED"),
        "nothing in the log was marked redacted, yet we wrote keys into it:\n{log}"
    );

    // Every key we wrote must have been redacted by *name*: the marker carries
    // the fingerprint, so this checks the right key was recognised rather than
    // some blanket scrub having fired.
    for (key, material) in keys {
        let fingerprint = fotw_secrets::Fingerprint::of(&SecretString::new(material.clone()));
        assert!(
            log.contains(fingerprint.as_str()),
            "{} was written to the log but never redacted by fingerprint",
            key.account()
        );
    }

    // The redaction must not have eaten the surrounding context, or the log is
    // useless and will be switched off.
    assert!(log.contains("Authorization: Token"), "context lost:\n{log}");
    assert!(
        log.contains("configured apikey:deepgram"),
        "context lost:\n{log}"
    );
}

/// Guard 3: prove the scan covered the files we care about.
fn assert_the_scan_actually_looked(report: &ScanReport, root: &Path) {
    assert!(
        report.files.len() >= 6,
        "scanned only {} files; expected the index, the WAL, the SHM, the log and the \
         settings at minimum",
        report.files.len()
    );
    assert!(report.bytes > 0, "scanned 0 bytes");

    for expected in [
        root.join("data").join("db.sqlite3"),
        root.join("data").join("db.sqlite3-wal"),
        root.join("data").join("db.sqlite3-shm"),
        root.join("config").join("credentials.json"),
        root.join("logs").join("fotwd.log"),
    ] {
        assert!(
            report.files.contains(&expected),
            "the scan never visited {}",
            expected.display()
        );
    }
}

// ------------------------------------------------------------ the test body

/// The whole acceptance run against one [`KeyStore`] implementation.
fn run_acceptance(store: &dyn KeyStore, label: &str) {
    let temp = tempfile::tempdir().expect("cannot create temp dir");
    let root = temp.path().join("fotw-root");
    let control = temp.path().join("control");
    fs::create_dir_all(&root).unwrap();

    let keys = test_keys();

    // Guard 1, first: if the scanner cannot find a planted key, stop.
    positive_control(&control, &keys);

    // The store must not create files under the app root. Snapshot around the
    // store operations specifically, so a plaintext fallback would show up as
    // a new path rather than being hidden among the log and index writes.
    let before = file_set(&root);
    for (key, material) in &keys {
        store
            .set(*key, &SecretString::new(material.clone()))
            .expect("store rejected a key");
    }
    let after = file_set(&root);
    assert_eq!(
        before, after,
        "[{label}] the key store created a file under the app root; \
         a KeyStore must never write one"
    );

    exercise_every_write_path(&root, store, &keys);

    assert_the_log_was_written_and_redacted(&root, &keys);

    let report = scan(&root, &keys);
    assert_the_scan_actually_looked(&report, &root);

    assert!(
        report.hits.is_empty(),
        "[{label}] KEY-01 violated: key material found on disk in {} place(s) \
         after scanning {} files ({} bytes):\n{}",
        report.hits.len(),
        report.files.len(),
        report.bytes,
        report.describe_hits()
    );

    // Clean up whatever we put in a real keychain.
    for (key, _) in &keys {
        let _ = store.delete(*key);
    }
}

/// KEY-01 against the in-memory store — the one that runs everywhere.
#[test]
fn no_key_material_reaches_disk_with_the_in_memory_store() {
    run_acceptance(&InMemoryKeyStore::new(), "in-memory");
}

/// KEY-01 against the real OS keychain.
///
/// Opt-in (`FOTW_KEYCHAIN_TESTS=1`): CI runners have no keychain, and on macOS
/// an unsigned test binary writing to the login keychain raises an interactive
/// unlock prompt that hangs the run rather than failing it.
#[test]
fn no_key_material_reaches_disk_with_the_os_keychain() {
    if !os_tests_enabled() {
        eprintln!("skipping: set FOTW_KEYCHAIN_TESTS=1 to run KEY-01 against the OS keychain");
        return;
    }
    let store = OsKeyStore::new().expect("FOTW_KEYCHAIN_TESTS=1 but no secret service");
    run_acceptance(&store, "os-keychain");
}

/// The guard itself, as a test, so a regression in [`positive_control`] is
/// reported as its own failure rather than as a confusing pass elsewhere.
#[test]
fn the_scanner_finds_keys_that_are_actually_there() {
    let temp = tempfile::tempdir().unwrap();
    let keys = test_keys();
    positive_control(&temp.path().join("control"), &keys);
}

/// A secret written *around* the redacting path lands on disk in the clear.
///
/// This is the negative control for the redactor: it pins that the protection
/// comes from the redacting sink and not from some accident of the fixture
/// (material that never really reaches the file, a log that is never written).
/// If this test ever starts failing, the acceptance test above has stopped
/// proving anything — the keys were not reaching disk in the first place.
#[test]
fn writing_around_the_redactor_does_leak_which_is_why_the_redactor_matters() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("unprotected");
    fs::create_dir_all(&root).unwrap();
    let keys = test_keys();

    // Same content as the acceptance log, written straight to the file.
    let mut raw = fs::File::create(root.join("unredacted.log")).unwrap();
    for (key, material) in &keys {
        writeln!(raw, "configured {} with key {material}", key.account()).unwrap();
    }
    drop(raw);

    let report = scan(&root, &keys);
    assert_eq!(
        report.hits.len(),
        keys.len(),
        "the same content that the redactor protects did NOT leak when written raw, \
         which means the acceptance test's fixture never had real key material in it"
    );
}
