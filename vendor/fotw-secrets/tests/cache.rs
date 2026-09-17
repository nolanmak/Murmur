//! Reading the OS credential store once per process, not once per call.
//!
//! # The bug this fixes
//!
//! `open_library` reads `db:masterkey` every time it is called, and it is
//! called by `list`, `serve`, `summarize`, persist, retention, import and
//! export. A single `fotwd record` touches the keychain twice; a
//! build-test-run cycle touches it a dozen times.
//!
//! On macOS every one of those touches is a separate approval dialog whenever
//! the calling binary's signature has changed — which, during development, is
//! after every single `cargo build`. Six dialogs in a row is not a signing bug
//! to be argued with; it is the same secret being fetched six times.
//!
//! # Why this is the same thing Electron does
//!
//! Chromium's `OSCrypt` — the "<App> Safe Storage" entries visible in any
//! macOS keychain — reads one item once at startup and keeps the key in
//! memory for the life of the process. It does not consult the keychain again
//! per secret. That is not a workaround; it is the design, and the reason
//! shipped Electron apps do not prompt in a loop.
//!
//! The secret is already in this process's memory while it is being used, so
//! holding it for the process lifetime widens the window, not the threat: the
//! model in docs/REQUIREMENTS.md §10 is a same-user process, and such a
//! process can read our memory whenever we hold the key at all.

use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};

use fotw_secrets::{CachedKeyStore, KeyStore, Provider, SecretKey, SecretString, SecretsError};

/// A store that counts how often it is actually consulted.
#[derive(Default)]
struct Counting {
    entries: Mutex<Vec<(SecretKey, String)>>,
    reads: AtomicU32,
    writes: AtomicU32,
}

impl Counting {
    fn with(key: SecretKey, value: &str) -> Self {
        let s = Self::default();
        s.entries.lock().unwrap().push((key, value.to_owned()));
        s
    }
    fn reads(&self) -> u32 {
        self.reads.load(Ordering::Relaxed)
    }
}

impl KeyStore for Counting {
    fn set(&self, key: SecretKey, secret: &SecretString) -> Result<(), SecretsError> {
        self.writes.fetch_add(1, Ordering::Relaxed);
        let mut e = self.entries.lock().unwrap();
        e.retain(|(k, _)| *k != key);
        e.push((key, secret.expose().to_owned()));
        Ok(())
    }

    fn get(&self, key: SecretKey) -> Result<SecretString, SecretsError> {
        self.reads.fetch_add(1, Ordering::Relaxed);
        self.entries
            .lock()
            .unwrap()
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, v)| SecretString::new(v.clone()))
            .ok_or(SecretsError::NotFound { key: key.account() })
    }

    fn delete(&self, key: SecretKey) -> Result<(), SecretsError> {
        self.writes.fetch_add(1, Ordering::Relaxed);
        self.entries.lock().unwrap().retain(|(k, _)| *k != key);
        Ok(())
    }

    fn contains(&self, key: SecretKey) -> Result<bool, SecretsError> {
        self.reads.fetch_add(1, Ordering::Relaxed);
        Ok(self.entries.lock().unwrap().iter().any(|(k, _)| *k == key))
    }
}

const MASTER: SecretKey = SecretKey::DbMasterKey;

#[test]
fn the_first_read_reaches_the_store() {
    let cached = CachedKeyStore::new(Counting::with(MASTER, "hunter2"));
    let got = cached.get(MASTER).expect("read");
    assert_eq!(got.expose(), "hunter2");
    assert_eq!(cached.inner().reads(), 1);
}

/// The whole point: six calls, one dialog.
#[test]
fn repeated_reads_do_not_reach_the_store_again() {
    let cached = CachedKeyStore::new(Counting::with(MASTER, "hunter2"));
    for _ in 0..6 {
        assert_eq!(cached.get(MASTER).expect("read").expose(), "hunter2");
    }
    assert_eq!(
        cached.inner().reads(),
        1,
        "each repeat is another approval dialog on macOS"
    );
}

/// `fotwd key list` asks about four providers. Absence has to be remembered
/// too, or a machine with no keys configured prompts four times and then
/// prompts four times again on the next command.
#[test]
fn a_missing_secret_is_remembered_as_missing() {
    let cached = CachedKeyStore::new(Counting::default());

    assert!(matches!(
        cached.get(MASTER),
        Err(SecretsError::NotFound { .. })
    ));
    assert!(matches!(
        cached.get(MASTER),
        Err(SecretsError::NotFound { .. })
    ));
    assert!(matches!(
        cached.get(MASTER),
        Err(SecretsError::NotFound { .. })
    ));

    assert_eq!(cached.inner().reads(), 1);
}

/// Caching absence must not break the first-run path, where the ceremony
/// stores a key moments after discovering there was none.
#[test]
fn writing_after_a_miss_makes_the_value_visible() {
    let cached = CachedKeyStore::new(Counting::default());
    assert!(cached.get(MASTER).is_err());

    cached
        .set(MASTER, &SecretString::new("freshly-minted"))
        .expect("set");

    assert_eq!(
        cached.get(MASTER).expect("read back").expose(),
        "freshly-minted",
        "a cached miss survived the write that resolved it"
    );
}

#[test]
fn a_write_is_reflected_without_another_read() {
    let cached = CachedKeyStore::new(Counting::with(MASTER, "old"));
    assert_eq!(cached.get(MASTER).unwrap().expose(), "old");
    let after_first = cached.inner().reads();

    cached.set(MASTER, &SecretString::new("new")).unwrap();

    assert_eq!(cached.get(MASTER).unwrap().expose(), "new");
    assert_eq!(cached.inner().reads(), after_first, "the write went stale");
}

#[test]
fn a_delete_is_reflected() {
    let cached = CachedKeyStore::new(Counting::with(MASTER, "doomed"));
    assert!(cached.get(MASTER).is_ok());

    cached.delete(MASTER).unwrap();

    assert!(
        matches!(cached.get(MASTER), Err(SecretsError::NotFound { .. })),
        "a deleted secret was still served from cache"
    );
}

#[test]
fn contains_is_answered_from_the_cache_too() {
    let cached = CachedKeyStore::new(Counting::with(MASTER, "present"));
    assert!(cached.get(MASTER).is_ok());
    let after_read = cached.inner().reads();

    assert!(cached.contains(MASTER).unwrap());
    assert!(cached.contains(MASTER).unwrap());

    assert_eq!(
        cached.inner().reads(),
        after_read,
        "contains() went to the store for something already held"
    );
}

/// Different accounts are cached independently — the Deepgram key must not be
/// served in place of the library key.
#[test]
fn accounts_do_not_collide() {
    let store = Counting::default();
    store
        .set(MASTER, &SecretString::new("master-material"))
        .unwrap();
    store
        .set(
            SecretKey::ApiKey(Provider::Deepgram),
            &SecretString::new("deepgram-material"),
        )
        .unwrap();

    let cached = CachedKeyStore::new(store);
    assert_eq!(cached.get(MASTER).unwrap().expose(), "master-material");
    assert_eq!(
        cached
            .get(SecretKey::ApiKey(Provider::Deepgram))
            .unwrap()
            .expose(),
        "deepgram-material"
    );
}

/// A store failure that is not "missing" must NOT be cached: a timed-out
/// approval dialog is exactly this case, and remembering it would turn one
/// unlucky moment into a process that never sees the key again.
#[test]
fn a_transient_failure_is_not_remembered() {
    struct FlakyOnce {
        calls: AtomicU32,
    }
    impl KeyStore for FlakyOnce {
        fn set(&self, _k: SecretKey, _s: &SecretString) -> Result<(), SecretsError> {
            Ok(())
        }
        fn get(&self, _k: SecretKey) -> Result<SecretString, SecretsError> {
            if self.calls.fetch_add(1, Ordering::Relaxed) == 0 {
                return Err(SecretsError::Platform {
                    operation: "reading",
                    key: "db:masterkey".to_owned(),
                    detail: "no answer within 5s".to_owned(),
                });
            }
            Ok(SecretString::new("arrived-late"))
        }
        fn delete(&self, _k: SecretKey) -> Result<(), SecretsError> {
            Ok(())
        }
        fn contains(&self, _k: SecretKey) -> Result<bool, SecretsError> {
            Ok(true)
        }
    }

    let cached = CachedKeyStore::new(FlakyOnce {
        calls: AtomicU32::new(0),
    });
    assert!(cached.get(MASTER).is_err(), "first call should fail");
    assert_eq!(
        cached
            .get(MASTER)
            .expect("retry must reach the store")
            .expose(),
        "arrived-late",
        "a transient platform error was cached as if it were an answer"
    );
}
