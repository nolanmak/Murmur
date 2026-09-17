//! Read the credential store once per process, not once per call.
//!
//! # The problem
//!
//! `open_library` reads `db:masterkey` every time it runs, and it runs for
//! `list`, `serve`, `summarize`, persist, retention, import and export. One
//! `fotwd record` touches the store twice. A build-test-run cycle touches it a
//! dozen times.
//!
//! On macOS a keychain item's ACL is bound to the code signature that created
//! it, so a binary that has been rebuilt is a different principal and every
//! touch raises a separate approval dialog. Six dialogs in a row is not a
//! signing problem to argue with — it is one secret being fetched six times.
//!
//! # This is the shape Chromium already uses
//!
//! The `<App> Safe Storage` entries in any macOS keychain — Slack's, Zoom's,
//! every Electron app's — are Chromium's `OSCrypt`: one item, read once at
//! startup, held in memory for the life of the process. It never consults the
//! keychain per secret. That is why shipped Electron apps do not prompt in a
//! loop, and it is not a workaround but the design.
//!
//! # What holding the key costs
//!
//! Nothing this process was not already paying. The material is in our address
//! space whenever it is being used; the threat model in docs/REQUIREMENTS.md
//! §10 is a same-user process, and such a process can read our memory at the
//! moment we hold the key at all. Caching widens the window, not the threat.
//! The cache is per-[`CachedKeyStore`], never global, so a test cannot
//! accidentally inherit another test's secrets.
//!
//! # What is deliberately not cached
//!
//! A [`SecretsError::Platform`] or [`SecretsError::AccessDenied`] is never
//! remembered. A timed-out approval dialog produces exactly those, and caching
//! one would turn a single unlucky moment into a process that can never see
//! the key again. Only a definite answer — a value, or a definite absence —
//! is worth keeping.

use std::collections::HashMap;
use std::sync::Mutex;

use crate::error::SecretsError;
use crate::keys::SecretKey;
use crate::secret::SecretString;
use crate::store::KeyStore;

/// What we know about one account.
enum Known {
    /// The material, as returned by the underlying store.
    Present(String),
    /// The store answered definitively that there is nothing here.
    Absent,
}

/// A [`KeyStore`] that remembers what the one underneath already told it.
///
/// Wraps rather than replaces, so the OS store keeps its single
/// responsibility and this stays testable without a real keychain — which
/// matters, because a test that touched the developer's keychain would raise
/// the very dialog this type exists to prevent.
pub struct CachedKeyStore<S: KeyStore> {
    inner: S,
    known: Mutex<HashMap<SecretKey, Known>>,
}

impl<S: KeyStore> std::fmt::Debug for CachedKeyStore<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never the contents, and never the key names: §10's never-log rule
        // does not stop at the log file.
        f.write_str("CachedKeyStore(<redacted>)")
    }
}

impl<S: KeyStore> CachedKeyStore<S> {
    /// Wrap `inner`, with nothing known yet.
    #[must_use]
    pub fn new(inner: S) -> Self {
        Self {
            inner,
            known: Mutex::new(HashMap::new()),
        }
    }

    /// The store underneath, for tests that assert how often it was consulted.
    #[must_use]
    pub fn inner(&self) -> &S {
        &self.inner
    }

    /// Forget everything, so the next call goes to the store again.
    ///
    /// For a caller that has reason to believe the store changed underneath
    /// us — another process running `fotwd key set`, say.
    pub fn forget(&self) {
        self.lock().clear();
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<SecretKey, Known>> {
        // A poisoned lock means another thread panicked mid-update. The map is
        // a cache: the worst a stale entry does is cost one extra read.
        self.known.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl<S: KeyStore> KeyStore for CachedKeyStore<S> {
    fn set(&self, key: SecretKey, secret: &SecretString) -> Result<(), SecretsError> {
        self.inner.set(key, secret)?;
        // Recorded only after the store accepted it. Caching a write that
        // failed would serve a value that is not there.
        self.lock()
            .insert(key, Known::Present(secret.expose().to_owned()));
        Ok(())
    }

    fn get(&self, key: SecretKey) -> Result<SecretString, SecretsError> {
        if let Some(known) = self.lock().get(&key) {
            return match known {
                Known::Present(material) => Ok(SecretString::new(material.clone())),
                Known::Absent => Err(SecretsError::NotFound { key: key.account() }),
            };
        }

        match self.inner.get(key) {
            Ok(secret) => {
                self.lock()
                    .insert(key, Known::Present(secret.expose().to_owned()));
                Ok(secret)
            }
            Err(SecretsError::NotFound { key: account }) => {
                // Absence is an answer, and `fotwd key list` asks it four
                // times in a row.
                self.lock().insert(key, Known::Absent);
                Err(SecretsError::NotFound { key: account })
            }
            // Everything else is the store failing, not answering.
            Err(other) => Err(other),
        }
    }

    fn delete(&self, key: SecretKey) -> Result<(), SecretsError> {
        self.inner.delete(key)?;
        self.lock().insert(key, Known::Absent);
        Ok(())
    }

    fn contains(&self, key: SecretKey) -> Result<bool, SecretsError> {
        if let Some(known) = self.lock().get(&key) {
            return Ok(matches!(known, Known::Present(_)));
        }
        let present = self.inner.contains(key)?;
        // A bare `contains` cannot fill in the material, so only absence is
        // recordable here; a later `get` still has to fetch a present one.
        if !present {
            self.lock().insert(key, Known::Absent);
        }
        Ok(present)
    }
}
