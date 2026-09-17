//! The [`KeyStore`] seam and its two backends.

use std::collections::BTreeMap;
use std::sync::Mutex;

use crate::{SecretKey, SecretString, SecretsError};

/// Somewhere secrets can be kept.
///
/// Two implementations: [`OsKeyStore`] (the real one, backed by the platform
/// credential store) and [`InMemoryKeyStore`] (tests). There is deliberately
/// **no file-backed implementation** — see KEY-05 and [`OsKeyStore`]. If one
/// is ever added, this doc comment is the place the argument for it has to be
/// written down and lost.
///
/// Object-safe: the pipeline holds a `Box<dyn KeyStore>` so the same code path
/// runs against the fake in CI and the keychain in production.
pub trait KeyStore: Send + Sync {
    /// Store `secret` under `key`, replacing any existing value.
    fn set(&self, key: SecretKey, secret: &SecretString) -> Result<(), SecretsError>;

    /// Read the secret stored under `key`.
    ///
    /// Returns [`SecretsError::NotFound`] if it was never set. Read on demand
    /// and let the result drop — docs/REQUIREMENTS.md 10 requires keys are
    /// "read on demand into a `SecretString` and zeroized on drop; never held
    /// in a global".
    fn get(&self, key: SecretKey) -> Result<SecretString, SecretsError>;

    /// Remove the secret stored under `key`.
    ///
    /// Removing something that is not there succeeds: callers reset a provider
    /// without checking first, and a spurious error there only teaches them to
    /// ignore the result.
    fn delete(&self, key: SecretKey) -> Result<(), SecretsError>;

    /// Whether a secret is stored under `key`.
    fn contains(&self, key: SecretKey) -> Result<bool, SecretsError>;
}

/// Whether the platform credential store can be used, and if not, why.
///
/// Exists as a type so [`OsKeyStore::with_probe`] can be driven from a test: a
/// hosted CI runner cannot be made to *not* have a keychain on demand, so the
/// KEY-05 failure path would otherwise be the one path that never runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoreAvailability {
    /// A credential store initialised successfully.
    Available,
    /// No usable credential store, with the platform's explanation.
    Unavailable(String),
}

/// The OS keychain: macOS Keychain Services, Windows Credential Manager,
/// Secret Service on Linux.
///
/// # KEY-05: this type refuses to exist rather than degrade
///
/// [`OsKeyStore::new`] probes the platform store *before* returning a value.
/// With no Secret Service it returns [`SecretsError::NoSecretService`] and no
/// `OsKeyStore` is constructed — so there is no object on which a caller could
/// invoke `set`, and therefore no code path from "no keychain" to "key written
/// somewhere else".
///
/// That structure is the point. The spec names Electron's `safeStorage` as the
/// anti-pattern: it hands back a working-looking object that encrypts with a
/// **hardcoded plaintext password**, and reports the degradation only through
/// a separate `getSelectedStorageBackend() === 'basic_text'` call. Every
/// caller who forgets that second call ships plaintext secrets and believes
/// otherwise. A control you have to remember to ask about is not a control; a
/// constructor that fails is.
///
/// `Debug` is safe to derive here precisely because the type is stateless: it
/// holds no handle, no service name, and above all no material.
#[derive(Debug)]
pub struct OsKeyStore {
    /// No state. The field exists so the struct cannot be built by a literal
    /// outside `with_probe`, which is what keeps the KEY-05 probe
    /// unskippable.
    _probe_passed: (),
}

impl OsKeyStore {
    /// Open the platform credential store, or fail.
    ///
    /// # Errors
    ///
    /// [`SecretsError::NoSecretService`] when no credential store is
    /// available. There is no third outcome and no fallback.
    pub fn new() -> Result<Self, SecretsError> {
        Self::with_probe(platform_availability)
    }

    /// Open the store using a caller-supplied availability probe.
    ///
    /// The seam that makes the KEY-05 failure path testable on a machine that
    /// does have a keychain. Production goes through [`OsKeyStore::new`].
    ///
    /// # Errors
    ///
    /// [`SecretsError::NoSecretService`] when the probe reports
    /// [`StoreAvailability::Unavailable`].
    pub fn with_probe(probe: impl FnOnce() -> StoreAvailability) -> Result<Self, SecretsError> {
        match probe() {
            StoreAvailability::Available => Ok(Self { _probe_passed: () }),
            StoreAvailability::Unavailable(why) => Err(SecretsError::NoSecretService(why)),
        }
    }
}

/// Ask the platform whether a credential store initialised.
///
/// `keyring::Entry::store_status()` is why this crate is on keyring 4 rather
/// than the flatter v3 API: it reports initialisation *without* attempting a
/// read or a write. A probe that had to write would either leave a test
/// credential in the user's keychain or, worse, discover the problem halfway
/// through a real `set()` — at which point we would have to guess whether a
/// partial credential was left behind.
fn platform_availability() -> StoreAvailability {
    match keyring::Entry::store_status() {
        Ok(()) => StoreAvailability::Available,
        Err(err) => StoreAvailability::Unavailable(err.to_string()),
    }
}

/// How long any single keychain call may take before we abandon it.
///
/// Not a performance guard — a liveness one. macOS keys a keychain item's ACL
/// to the calling code's designated requirement. A binary signed ad-hoc, or
/// signed under a different code identifier than the one that created the
/// item, is a *different principal*, so the system raises a modal approval
/// dialog. Under launchd, in CI, over SSH, or from a `cargo run` in a script
/// there is nobody to click it: the call blocks forever, with no output and no
/// error. A daemon that hangs silently on startup is worse than one that
/// refuses to start.
pub const KEYCHAIN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Whether anyone could actually answer an approval dialog right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Answerable {
    /// A person at this machine, with a window server that can draw a dialog.
    Person,
    /// launchd, CI, or SSH. The dialog either cannot be drawn or cannot be
    /// seen, so waiting only converts a fast error into a silent hang.
    Nobody,
}

/// How long to wait for the credential store, given who could answer.
///
/// The old single five-second deadline was right for [`Answerable::Nobody`]
/// and wrong for everyone else: nobody finds an approval window, reads it and
/// clicks the correct button inside five seconds. Because a timed-out request
/// is abandoned rather than cancelled, the user's eventual click then landed
/// on a request nothing was waiting for — they clicked Allow and the command
/// had already failed.
///
/// Still bounded in both cases. "Wait forever" is the hang this exists to
/// prevent, and an interactive session can still be one where the dialog
/// never draws.
#[must_use]
pub fn keychain_timeout(who: Answerable) -> std::time::Duration {
    match who {
        Answerable::Person => std::time::Duration::from_secs(60),
        Answerable::Nobody => KEYCHAIN_TIMEOUT,
    }
}

/// Decide from environment markers and whether a GUI session exists.
///
/// Split from the lookup so it is testable: a test cannot remove the window
/// server, and setting process-wide environment variables from one test races
/// every other test in the binary.
#[must_use]
pub fn answerable_from(env: &[(&str, &str)], has_window_server: bool) -> Answerable {
    let marked = |name: &str| env.iter().any(|(k, v)| *k == name && !v.trim().is_empty());

    // CI first: a hosted macOS runner has a window server and still nobody to
    // click. SSH next: a person, but not one who can see the far end's screen.
    if marked("CI") || marked("SSH_CONNECTION") || marked("SSH_TTY") {
        return Answerable::Nobody;
    }
    if has_window_server {
        Answerable::Person
    } else {
        Answerable::Nobody
    }
}

/// [`answerable_from`], reading this process's actual environment.
#[must_use]
pub fn answerable() -> Answerable {
    let env: Vec<(String, String)> = ["CI", "SSH_CONNECTION", "SSH_TTY"]
        .iter()
        .filter_map(|k| std::env::var(k).ok().map(|v| ((*k).to_owned(), v)))
        .collect();
    let borrowed: Vec<(&str, &str)> = env.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    answerable_from(&borrowed, has_window_server())
}

/// Whether this process is in a GUI session that could draw a dialog.
///
/// `SecurityAgent` runs in the Aqua session, so a process outside one cannot
/// be shown anything. Detected from the session's bootstrap namespace rather
/// than from a TTY, because the app bundle is launched by LaunchServices and
/// has no TTY at all while very much having a screen.
fn has_window_server() -> bool {
    #[cfg(target_os = "macos")]
    {
        // Set for every process in a GUI login session and absent in the
        // system context that launchd daemons run in.
        std::env::var_os("XPC_SERVICE_NAME").is_some()
            || std::env::var_os("__CFBundleIdentifier").is_some()
            || std::env::var_os("TERM_SESSION_ID").is_some()
            || std::env::var_os("SECURITYSESSIONID").is_some()
    }
    #[cfg(not(target_os = "macos"))]
    {
        false
    }
}

/// Run one keychain call, giving up after [`KEYCHAIN_TIMEOUT`].
///
/// # Why the thread is detached and never joined
///
/// The obvious shape — `std::thread::scope` — is wrong here, and wrong in a
/// way that *looks* right and passes a green test suite. `scope` joins every
/// thread it spawned before returning, so `recv_timeout` fires correctly after
/// five seconds and then the scope blocks forever waiting for the very call we
/// just gave up on. The timeout becomes decorative. That bug shipped here and
/// was caught only by sampling a hung process: the main thread sat in
/// `thread::park` under `thread::scope`, one frame below a timeout that had
/// already expired.
///
/// So the worker is detached. It is stuck inside a system call we cannot
/// cancel — there is no `SecKeychainFindGenericPasswordWithTimeout` — and the
/// honest options are to abandon it or to hang. We abandon it: the send half
/// finds no receiver and the result, including any `SecretString`, is dropped
/// and zeroed on that thread. The cost is one parked thread for the remaining
/// life of a process that is, by construction, on its way to reporting an
/// error.
fn with_deadline<T, F>(operation: &'static str, account: String, work: F) -> Result<T, SecretsError>
where
    F: FnOnce() -> Result<T, SecretsError> + Send + 'static,
    T: Send + 'static,
{
    with_deadline_after(keychain_timeout(answerable()), operation, account, work)
}

/// [`with_deadline`] with the deadline named, so a test can use a short one.
///
/// Split out only for that: a test that had to wait the real five seconds to
/// prove the guard works would be a test nobody runs.
fn with_deadline_after<T, F>(
    timeout: std::time::Duration,
    operation: &'static str,
    account: String,
    work: F,
) -> Result<T, SecretsError>
where
    F: FnOnce() -> Result<T, SecretsError> + Send + 'static,
    T: Send + 'static,
{
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        // `send` fails once we have timed out and dropped the receiver. That
        // is the expected path, not an error, and dropping the value here is
        // what zeroes it.
        let _ = tx.send(work());
    });
    rx.recv_timeout(timeout).unwrap_or_else(|_| {
        Err(SecretsError::Platform {
            operation,
            key: account,
            detail: format!(
                "no answer within {}s. This usually means macOS is showing an \
                 approval dialog that nothing can display: the keychain item's ACL \
                 is bound to the code signature that created it, and this binary \
                 presents a different one. An ad-hoc signature mints a new identity \
                 on every rebuild, and signing two binaries of the same product \
                 under different code identifiers makes them different principals \
                 for the same item. Build through `just dev-sign`, which signs \
                 every binary under one stable identifier, or approve the dialog \
                 once in a foreground session.",
                timeout.as_secs()
            ),
        })
    })
}

impl KeyStore for OsKeyStore {
    fn set(&self, key: SecretKey, secret: &SecretString) -> Result<(), SecretsError> {
        if secret.is_empty() {
            return Err(SecretsError::InvalidKeyMaterial(
                "refusing to store an empty secret".to_owned(),
            ));
        }
        let account = key.account();
        // Writes prompt too. Creating a new item does not, but *updating* one
        // whose ACL names a different principal does, so `set` can hang for
        // exactly the same reason `get` can.
        let material = secret.expose().to_owned();
        let for_thread = account.clone();
        with_deadline("writing", account, move || {
            keyring::Entry::new(key.service(), &for_thread)
                .and_then(|entry| entry.set_password(&material))
                .map_err(|err| SecretsError::from_keyring("writing", &for_thread, err))
        })
    }

    fn get(&self, key: SecretKey) -> Result<SecretString, SecretsError> {
        let account = key.account();
        let for_thread = account.clone();
        // `get_password` hands back a bare `String` — a copy of the material
        // outside `SecretString`'s protection. Wrap it in the same expression
        // so it is never bound to a name that could outlive this line, and so
        // the buffer is zeroed when the `SecretString` drops. That still holds
        // on the worker thread: if we have already timed out, the `SecretString`
        // is dropped there and zeroed there.
        with_deadline("reading", account, move || {
            keyring::Entry::new(key.service(), &for_thread)
                .and_then(|entry| entry.get_password())
                .map(SecretString::new)
                .map_err(|err| SecretsError::from_keyring("reading", &for_thread, err))
        })
    }

    fn delete(&self, key: SecretKey) -> Result<(), SecretsError> {
        let account = key.account();
        let for_thread = account.clone();
        with_deadline("deleting", account, move || {
            let entry = keyring::Entry::new(key.service(), &for_thread)
                .map_err(|err| SecretsError::from_keyring("deleting", &for_thread, err))?;
            match entry.delete_credential() {
                Ok(()) => Ok(()),
                // Deleting something that is not there is the desired state,
                // not a failure.
                Err(keyring::Error::NoEntry) => Ok(()),
                Err(err) => Err(SecretsError::from_keyring("deleting", &for_thread, err)),
            }
        })
    }

    fn contains(&self, key: SecretKey) -> Result<bool, SecretsError> {
        // There is no existence check in the keyring API that does not read
        // the credential, so this materialises the secret and immediately
        // drops it — zeroing the buffer on the way out. Prefer `get` when you
        // are going to need the value anyway; this is for the settings screen,
        // which only needs the boolean.
        match self.get(key) {
            Ok(_) => Ok(true),
            Err(err) if err.is_not_found() => Ok(false),
            Err(err) => Err(err),
        }
    }
}

/// A [`KeyStore`] that keeps secrets in memory and touches nothing else.
///
/// Carries the behavioural coverage for the whole trait, because the OS
/// backends cannot run on a CI box with no keychain and no D-Bus. It is also
/// what the KEY-01 acceptance test writes through: a store that provably never
/// opens a file is the right control for a test asserting no file contains a
/// key.
#[derive(Default)]
pub struct InMemoryKeyStore {
    entries: Mutex<BTreeMap<SecretKey, SecretString>>,
}

impl InMemoryKeyStore {
    /// An empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Lock the map, ignoring poisoning.
    ///
    /// A panic in another thread while holding this lock cannot have left the
    /// map inconsistent — every operation is a single map call — and refusing
    /// to serve keys afterwards would turn an unrelated panic into a failed
    /// recording.
    fn entries(&self) -> std::sync::MutexGuard<'_, BTreeMap<SecretKey, SecretString>> {
        self.entries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

impl KeyStore for InMemoryKeyStore {
    fn set(&self, key: SecretKey, secret: &SecretString) -> Result<(), SecretsError> {
        if secret.is_empty() {
            return Err(SecretsError::InvalidKeyMaterial(
                "refusing to store an empty secret".to_owned(),
            ));
        }
        self.entries().insert(key, secret.clone());
        Ok(())
    }

    fn get(&self, key: SecretKey) -> Result<SecretString, SecretsError> {
        self.entries()
            .get(&key)
            .cloned()
            .ok_or_else(|| SecretsError::NotFound { key: key.account() })
    }

    fn delete(&self, key: SecretKey) -> Result<(), SecretsError> {
        self.entries().remove(&key);
        Ok(())
    }

    fn contains(&self, key: SecretKey) -> Result<bool, SecretsError> {
        Ok(self.entries().contains_key(&key))
    }
}

/// Whether tests that touch the real OS keychain should run.
///
/// Off unless `FOTW_KEYCHAIN_TESTS=1`. `cargo test --workspace` has to pass on
/// a hosted runner with no keychain, no D-Bus session and no secrets
/// (docs/REQUIREMENTS.md 5.6), and on macOS an unsigned test binary writing to
/// the login keychain raises an interactive unlock prompt that would hang CI
/// rather than fail it.
#[must_use]
pub fn os_tests_enabled() -> bool {
    std::env::var("FOTW_KEYCHAIN_TESTS").is_ok_and(|value| value == "1")
}

#[cfg(test)]
mod tests {
    use crate::{
        InMemoryKeyStore, KeyStore, OsKeyStore, Provider, SecretKey, SecretString, SecretsError,
        StoreAvailability, os_tests_enabled,
    };

    // ---------------------------------------------------------------- seam

    /// The pipeline holds a `Box<dyn KeyStore>` so it can be handed the
    /// in-memory backend under test and the OS backend in production. If the
    /// trait stops being object-safe this fails to compile, which is the
    /// point.
    #[test]
    fn keystore_is_object_safe() {
        let store: Box<dyn KeyStore> = Box::new(InMemoryKeyStore::new());
        assert!(!store.contains(SecretKey::DbMasterKey).unwrap());
    }

    // ------------------------------------------------------------ in-memory

    #[test]
    fn round_trips_every_known_key() {
        let store = InMemoryKeyStore::new();

        for key in SecretKey::ALL {
            let material = format!("material-for-{}", key.account());
            store
                .set(key, &SecretString::new(material.clone()))
                .unwrap();
            assert!(store.contains(key).unwrap());
            assert_eq!(store.get(key).unwrap().expose(), material);
        }
    }

    #[test]
    fn get_of_an_unset_key_is_not_found() {
        let store = InMemoryKeyStore::new();
        let err = store
            .get(SecretKey::ApiKey(Provider::Deepgram))
            .unwrap_err();
        assert!(
            matches!(err, SecretsError::NotFound { .. }),
            "expected NotFound, got {err:?}"
        );
        assert!(err.is_not_found());
        // The error names the key, never the key material.
        assert!(err.to_string().contains("apikey:deepgram"));
    }

    #[test]
    fn set_overwrites_and_delete_removes() {
        let store = InMemoryKeyStore::new();
        let key = SecretKey::ApiKey(Provider::OpenAi);

        store.set(key, &SecretString::new("first")).unwrap();
        store.set(key, &SecretString::new("second")).unwrap();
        assert_eq!(store.get(key).unwrap().expose(), "second");

        store.delete(key).unwrap();
        assert!(!store.contains(key).unwrap());
        assert!(store.get(key).unwrap_err().is_not_found());

        // Deleting what is not there is not an error: callers reset a
        // provider without first checking, and a spurious failure there just
        // teaches them to ignore the result.
        store.delete(key).unwrap();
    }

    #[test]
    fn keys_are_isolated_from_each_other() {
        let store = InMemoryKeyStore::new();
        store
            .set(
                SecretKey::ApiKey(Provider::Deepgram),
                &SecretString::new("dg"),
            )
            .unwrap();
        store
            .set(
                SecretKey::ApiKey(Provider::Anthropic),
                &SecretString::new("an"),
            )
            .unwrap();

        assert_eq!(
            store
                .get(SecretKey::ApiKey(Provider::Deepgram))
                .unwrap()
                .expose(),
            "dg"
        );
        assert!(!store.contains(SecretKey::ApiKey(Provider::OpenAi)).unwrap());
    }

    // ------------------------------------------------- KEY-05: no fallback

    /// KEY-05. With no secret service, construction fails and no store
    /// exists to write through. Contrast Electron's `safeStorage`, which
    /// silently swaps in a hardcoded plaintext password and reports it only
    /// via a getter nobody calls.
    #[test]
    fn os_store_refuses_to_construct_without_a_secret_service() {
        let result = OsKeyStore::with_probe(|| {
            StoreAvailability::Unavailable("no D-Bus session bus".to_owned())
        });

        let err = result.expect_err("constructed a key store with no secret service");
        assert!(
            matches!(err, SecretsError::NoSecretService(_)),
            "expected NoSecretService, got {err:?}"
        );
        assert!(err.is_no_secret_service());
        assert!(
            err.to_string().contains("no D-Bus session bus"),
            "the user cannot fix what we will not name: {err}"
        );
    }

    /// The failure has to be *loud*, not a degraded object. There is no
    /// `OsKeyStore` value to call `set` on, so there is no code path from a
    /// missing secret service to a plaintext write. This test asserts the
    /// consequence: the error type carries no store.
    #[test]
    fn a_failed_probe_yields_no_store_at_all() {
        let probed = OsKeyStore::with_probe(|| StoreAvailability::Unavailable("locked".to_owned()));
        assert!(probed.is_err());

        // And the happy path does yield one, so the test above is not passing
        // because `with_probe` always fails.
        let available = OsKeyStore::with_probe(|| StoreAvailability::Available);
        assert!(available.is_ok(), "an available probe must produce a store");
    }

    /// On a real machine `new()` may legitimately go either way — a developer
    /// laptop has a keychain, a hosted CI runner does not. What is *not*
    /// allowed is any third outcome: no silent success with a degraded
    /// backend, and no error that is not the one the user can act on.
    #[test]
    fn os_store_construction_either_succeeds_or_reports_no_secret_service() {
        match OsKeyStore::new() {
            Ok(_) => {}
            Err(err) => assert!(
                err.is_no_secret_service(),
                "construction failed for a reason the user cannot act on: {err:?}"
            ),
        }
    }

    // ------------------------------------------- opt-in real keychain test

    /// The OS backend against the real platform store. Skipped unless
    /// `FOTW_KEYCHAIN_TESTS=1`, because CI runners have no keychain and, on
    /// macOS, an unsigned test binary triggers an interactive unlock prompt
    /// that would hang the run.
    #[test]
    fn os_store_round_trips_against_the_real_keychain() {
        if !os_tests_enabled() {
            eprintln!("skipping: set FOTW_KEYCHAIN_TESTS=1 to exercise the OS keychain");
            return;
        }

        let store = OsKeyStore::new().expect("FOTW_KEYCHAIN_TESTS=1 but no secret service");
        let key = SecretKey::ApiKey(Provider::Deepgram);
        let material = "fotw-test-value-please-delete";

        store.set(key, &SecretString::new(material)).unwrap();
        assert!(store.contains(key).unwrap());
        assert_eq!(store.get(key).unwrap().expose(), material);

        store.delete(key).unwrap();
        assert!(!store.contains(key).unwrap());
    }
}

#[cfg(test)]
mod deadline_tests {
    use super::*;

    /// The guard must return even when the work never does.
    ///
    /// This is the test the original implementation would have failed. It used
    /// `std::thread::scope`, which joins on exit, so the receive timed out
    /// correctly and then the scope blocked forever one frame below it — a
    /// hang that no unit test caught, because no unit test made the keychain
    /// actually block. If this test hangs, the guard has regressed to that.
    #[test]
    fn a_call_that_never_returns_still_times_out() {
        let started = std::time::Instant::now();
        let result: Result<u8, SecretsError> = with_deadline_after(
            std::time::Duration::from_millis(80),
            "reading",
            "db:masterkey".to_owned(),
            || {
                // Stands in for a keychain call parked on an approval dialog
                // that nothing can display.
                std::thread::sleep(std::time::Duration::from_secs(3_600));
                Ok(0)
            },
        );

        let waited = started.elapsed();
        assert!(
            matches!(result, Err(SecretsError::Platform { .. })),
            "expected a Platform error, got {result:?}"
        );
        assert!(
            waited < std::time::Duration::from_secs(5),
            "the guard did not actually give up: waited {waited:?}"
        );
    }

    /// The message has to name the cause, or it sends the reader hunting a
    /// database bug when the problem is a code signature.
    #[test]
    fn the_timeout_explains_that_it_is_a_signing_problem() {
        let result: Result<u8, SecretsError> = with_deadline_after(
            std::time::Duration::from_millis(20),
            "reading",
            "db:masterkey".to_owned(),
            || {
                std::thread::sleep(std::time::Duration::from_secs(3_600));
                Ok(0)
            },
        );
        let message = result.unwrap_err().to_string();
        assert!(message.contains("signature"), "unhelpful: {message}");
        assert!(message.contains("dev-sign"), "no way forward: {message}");
    }

    /// The happy path must not pay for the guard.
    #[test]
    fn work_that_completes_returns_its_value_promptly() {
        let started = std::time::Instant::now();
        let result = with_deadline_after(
            std::time::Duration::from_secs(30),
            "reading",
            "db:masterkey".to_owned(),
            || Ok(7u8),
        );
        assert_eq!(result.unwrap(), 7);
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
    }

    /// A worker that panics must surface as an error rather than hanging until
    /// the deadline: the sender is dropped, so the channel disconnects.
    #[test]
    fn a_panicking_call_fails_fast_instead_of_waiting_out_the_clock() {
        let started = std::time::Instant::now();
        let result: Result<u8, SecretsError> = with_deadline_after(
            std::time::Duration::from_secs(30),
            "reading",
            "db:masterkey".to_owned(),
            || panic!("the platform library exploded"),
        );
        assert!(result.is_err());
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "a panicking worker should disconnect the channel, not wait"
        );
    }
}
