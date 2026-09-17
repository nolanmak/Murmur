//! `fotw-secrets` — the only place a FlyOnTheWall API key exists in memory.
//!
//! See docs/REQUIREMENTS.md 10 and 4.5 (KEY-01 … KEY-08).
//!
//! The claim this crate has to make true is KEY-01: *keys live in the OS
//! keychain and are structurally barred from SQLite, logs, and crash
//! payloads*. "Structurally" is the load-bearing word. A rule that says
//! "remember not to log the key" is not a control; the controls here are:
//!
//! 1. [`SecretString`] is the only type that holds key material, its `Debug`
//!    and `Display` print a redaction, and there is **no `Deref<Target=str>`**
//!    — the material comes out through [`SecretString::expose`] and nowhere
//!    else, so `rg 'expose\('` enumerates every use site in the tree.
//! 2. [`CredentialRecord`] is what the database is allowed to hold. It has no
//!    field that can carry key material — only a [`Fingerprint`], the first
//!    16 hex chars of SHA-256, which tells the UI *which* key is configured
//!    and tells an attacker nothing.
//! 3. [`Redactor`] rewrites any string containing a live key before it reaches
//!    a log sink, replacing it with that key's fingerprint, and strips the
//!    four credential headers named in docs/REQUIREMENTS.md 10 by name
//!    regardless of their contents. [`RedactingWriter`] makes that a property
//!    of the sink rather than of every call site. (It holds the material, not
//!    only fingerprints — see [`Redactor`] for why substring redaction
//!    requires that, and why this crate is the right place for it.)
//! 4. [`KeyStore`] never writes a file. The OS backend hands material to the
//!    platform credential store; the in-memory backend keeps it in a
//!    `SecretString`. Neither has a filesystem fallback, which is what makes
//!    KEY-05 (below) enforceable rather than aspirational.
//!
//! # The one file this crate does write
//!
//! [`recovery`] writes `db.sqlite3.recovery`: the database master key **sealed
//! under a Recovery Key the user wrote down**. That is not a hole in rule 4, it
//! is the deliberate complement to it — a wrapped key on disk is what makes the
//! keychain survivable, and rule 4 is about *material*, which that file never
//! contains. The argument is set out in full in [`recovery`]'s module docs; the
//! short version is that a recovery path stored in the keychain is lost with
//! the keychain, which is the only failure it exists for.
//!
//! # KEY-05: no silent plaintext fallback
//!
//! On Linux with no Secret Service, [`OsKeyStore::new`] returns
//! [`SecretsError::NoSecretService`] and the store is never constructed. It
//! does not degrade to a file, a hardcoded password, or an "insecure mode"
//! flag. The contrast in the spec is Electron's `safeStorage`, which falls
//! back to a *hardcoded plaintext password* and signals it only through a
//! separate `getSelectedStorageBackend()` getter that callers forget to check.
//! A control the caller has to remember to ask about is not a control.
//!
//! # Testing without a keychain
//!
//! [`InMemoryKeyStore`] implements the whole [`KeyStore`] contract with no
//! platform involvement, so every behavioural test runs on a CI box with no
//! keychain, no D-Bus and no secrets. The OS backend's own tests are gated
//! behind `FOTW_KEYCHAIN_TESTS=1` (see [`os_tests_enabled`]) because a hosted
//! runner has nowhere to put a credential.

#![warn(missing_docs)]

pub mod cache;
mod error;
mod fingerprint;
mod index;
mod keys;
pub mod recovery;
mod redact;
mod secret;
mod store;
pub mod validate;

pub use crate::cache::CachedKeyStore;
pub use crate::error::SecretsError;
pub use crate::fingerprint::Fingerprint;
pub use crate::index::CredentialRecord;
pub use crate::keys::{KEYRING_SERVICE, Provider, SecretKey};
pub use crate::redact::{REDACTED, RedactingWriter, Redactor, SENSITIVE_HEADERS};
pub use crate::secret::SecretString;
pub use crate::store::{
    Answerable, InMemoryKeyStore, KEYCHAIN_TIMEOUT, KeyStore, OsKeyStore, StoreAvailability,
    answerable, answerable_from, keychain_timeout, os_tests_enabled,
};
