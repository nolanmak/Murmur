//! The Recovery Key: the second, offline way into the meeting library.
//!
//! See docs/REQUIREMENTS.md 10 and issue #38.
//!
//! # The problem this solves
//!
//! The 32-byte database master key lives in the OS keychain and, before this
//! module, that was its *only* copy. A wiped machine, a corrupted keychain, a
//! restore onto new hardware, or a macOS ACL that no longer matches the code
//! signature that created the item (issue #53) each turn the entire library
//! into ciphertext nobody can read. There was no recovery path, and the warning
//! `fotwd` printed about backing the key up named a thing the user could not
//! actually do.
//!
//! # The shape of the fix, and why it is a *wrapping*
//!
//! The Recovery Key does not replace the master key — it **unwraps** it:
//!
//! ```text
//!   Recovery Key (16 bytes, shown once)
//!         |  Argon2id(salt, m=64MiB, t=3, p=1)
//!         v
//!       KEK (32 bytes)
//!         |  XChaCha20-Poly1305 open
//!         v
//!   master key (32 bytes) ------> PRAGMA key
//!         ^
//!         |  the OS keychain, on a normal run
//! ```
//!
//! Two consequences, and both are the reason for the design:
//!
//! * The library opens **identically** either way. There is one ciphertext,
//!   one `PRAGMA key`, one set of pages. Recovery is not a second format with
//!   a second set of bugs.
//! * Rotating the Recovery Key rewrites a 200-byte file and **does not
//!   re-encrypt the database**. A design that showed the master key itself as
//!   the recovery string — which is what issue #38 literally proposes — cannot
//!   do that: rotating would mean `PRAGMA rekey` over the whole library, so in
//!   practice nobody would ever rotate.
//!
//! # Where the wrapped blob lives, and why that is the hard part
//!
//! **Next to the database, in `db.sqlite3.recovery`.** Not in the keychain.
//!
//! That is the whole point. The failure this feature exists for is *the
//! keychain is gone*; a blob stored in the keychain would be gone with it, and
//! the recovery path would be a key-shaped object that has never once been
//! useful. It has to sit somewhere that survives independently, and the only
//! such place we control is the data root — which is also the thing users back
//! up, because it is where their meetings are.
//!
//! The cost is stated plainly: **anyone who can read the disk has the wrapped
//! key.** docs/REQUIREMENTS.md 10.1 already puts same-user local malware out of
//! scope, and §10's stated threat model is unencrypted backups and other
//! user-space apps — so the blob travels into exactly the places the threat
//! model cares about. That is precisely why the KDF parameters below are not
//! decorative, and why the file is written 0600.
//!
//! # Never printed, never logged
//!
//! [`RecoveryKey`] has no `Display`, its `Debug` redacts, and the only way to
//! render it is [`RecoveryKey::display_string`], which hands back a
//! [`SecretString`]. `rg 'display_string\('` enumerates every place the key can
//! become visible; there is exactly one, in the first-run ceremony.

use std::fmt;
use std::path::{Path, PathBuf};

use crate::SecretString;

mod blob;
mod encoding;
mod error;

pub use self::blob::{KdfParams, WrappedMasterKey};
pub use self::error::RecoveryError;

/// Bytes of entropy in a Recovery Key.
///
/// 16, not 32. The key is transcribed by a human, by hand, probably in a hurry,
/// and every extra character is another chance to write down a `0` that reads
/// back as an `o`. 128 bits is beyond exhaustive search by any margin that will
/// ever matter, and the keychain copy of the master key is still the full 256
/// bits — this only bounds the strength of the *offline* path, against an
/// attacker who already has the blob.
pub const RECOVERY_KEY_BYTES: usize = 16;

/// Characters per display group.
pub const GROUP_LEN: usize = 4;

/// Display groups after the `fotw1` prefix.
///
/// 16 bytes is 26 bech32 characters plus a 6-character checksum: 32, which is
/// exactly eight groups of four. That is not a coincidence we arranged, but it
/// is why 16 bytes reads as well as it does.
pub const GROUP_COUNT: usize = 8;

/// The human-readable part of the encoded key.
pub const HRP: &str = "fotw";

/// 16 bytes of CSPRNG output that the user writes down.
///
/// Holds material, so it behaves like [`SecretString`]: redacting `Debug`, no
/// `Display`, no `PartialEq` (see [`RecoveryKey::ct_eq`]), zeroized on drop.
pub struct RecoveryKey([u8; RECOVERY_KEY_BYTES]);

/// The 32-byte database master key, outside `fotw-store`.
///
/// A near-twin of `fotw_store::DbKey`, and deliberately not that type: this
/// crate must not depend on the store (the store is what holds meeting text,
/// and `fotw-secrets` is meant to be the leaf everything else can depend on).
/// The duplication is ~30 lines of redaction and one volatile write loop.
pub struct MasterKeyBytes([u8; 32]);

impl RecoveryKey {
    /// Draw a fresh Recovery Key from the OS CSPRNG.
    ///
    /// # Errors
    ///
    /// [`RecoveryError::Crypto`] if the operating system has no entropy
    /// available, which is not a condition to paper over with a fallback.
    pub fn generate() -> Result<Self, RecoveryError> {
        Ok(Self(blob::os_random()?))
    }

    /// Wrap raw key bytes. For tests and for [`Self::parse`].
    #[must_use]
    pub fn from_bytes(bytes: [u8; RECOVERY_KEY_BYTES]) -> Self {
        Self(bytes)
    }

    /// Parse what a human typed back in.
    ///
    /// Forgiving about presentation — case, spaces, dashes, and the confusable
    /// characters bech32's alphabet already excludes — and unforgiving about
    /// content: the BCH checksum has to pass. See [`encoding`] for the full
    /// argument.
    ///
    /// # Errors
    ///
    /// [`RecoveryError::Malformed`] for anything that is not a well-formed
    /// Recovery Key. **This is a typo, not a wrong key**, and the two are
    /// different errors on purpose: nothing has been tried against the library
    /// yet, so the user needs to be told to check what they typed rather than
    /// to go looking for their other backup.
    pub fn parse(typed: &str) -> Result<Self, RecoveryError> {
        encoding::decode(typed).map(Self)
    }

    /// Render the key for the one screen that shows it.
    ///
    /// Returns a [`SecretString`] so that a stray `{:?}` on the result still
    /// redacts, and so the material is zeroized when the display is done with
    /// it. Grouped `fotw1-xxxx-xxxx-…` for transcription.
    ///
    /// # Errors
    ///
    /// [`RecoveryError::Crypto`] only if bech32 encoding fails, which for a
    /// fixed 16-byte payload and a 4-character HRP it cannot.
    pub fn display_string(&self) -> Result<SecretString, RecoveryError> {
        encoding::encode_grouped(&self.0).map(SecretString::new)
    }

    /// The key material, for the KDF.
    ///
    /// Named like [`SecretString::expose`] so the same `rg` audit finds it.
    #[must_use]
    pub fn expose(&self) -> &[u8; RECOVERY_KEY_BYTES] {
        &self.0
    }

    /// Whether the user typed group `index` (0-based) back correctly.
    ///
    /// The confirmation challenge. Takes the typed text rather than handing out
    /// the expected group, so no caller can accidentally print the answer while
    /// asking the question.
    ///
    /// # Errors
    ///
    /// [`RecoveryError::Crypto`] if the key cannot be rendered, or
    /// [`RecoveryError::Malformed`] if `index` is out of range.
    pub fn group_matches(&self, index: usize, typed: &str) -> Result<bool, RecoveryError> {
        encoding::group_matches(&self.0, index, typed)
    }

    /// Constant-time equality.
    ///
    /// [`RecoveryKey`] has no `PartialEq` for the same reason [`SecretString`]
    /// has none: a short-circuiting compare over key material is a timing
    /// oracle, so there is no safe default and therefore no default.
    #[must_use]
    pub fn ct_eq(&self, other: &Self) -> bool {
        ct_eq_bytes(&self.0, &other.0)
    }
}

impl MasterKeyBytes {
    /// Wrap 32 bytes of master key material.
    #[must_use]
    pub fn new(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Draw a fresh master key from the OS CSPRNG.
    ///
    /// # Errors
    ///
    /// [`RecoveryError::Crypto`] if the operating system has no entropy. This
    /// is the one place in the program where a weak source would be
    /// catastrophic *and* unnoticeable, so it is an error rather than a
    /// fallback.
    pub fn generate() -> Result<Self, RecoveryError> {
        Ok(Self(blob::os_random()?))
    }

    /// Wrap material of unknown length, rejecting anything but 32 bytes.
    ///
    /// SQLCipher silently zero-pads a short raw key rather than failing, so a
    /// truncated read has to be an error here or it becomes a weaker database
    /// that still opens.
    ///
    /// # Errors
    ///
    /// [`RecoveryError::Crypto`] when the slice is not 32 bytes long.
    pub fn from_slice(bytes: &[u8]) -> Result<Self, RecoveryError> {
        let arr: [u8; 32] = bytes.try_into().map_err(|_| {
            RecoveryError::Crypto(format!(
                "the master key must be 32 bytes, got {}",
                bytes.len()
            ))
        })?;
        Ok(Self(arr))
    }

    /// The material. One call site per consumer, and greppable.
    #[must_use]
    pub fn expose(&self) -> &[u8; 32] {
        &self.0
    }

    /// Constant-time equality, for the same reason as [`RecoveryKey::ct_eq`].
    #[must_use]
    pub fn ct_eq(&self, other: &Self) -> bool {
        ct_eq_bytes(&self.0, &other.0)
    }
}

/// The canonical location of the sealed blob for a library at `db_path`.
///
/// `db.sqlite3.recovery`, beside `db.sqlite3`. Named so that the two travel
/// together: anything that copies `db.sqlite3*` — which is what a person does
/// by hand, because of `-wal` and `-shm` — takes the recovery file with it.
#[must_use]
pub fn blob_path_for(db_path: &Path) -> PathBuf {
    let mut name = db_path.as_os_str().to_os_string();
    name.push(".recovery");
    PathBuf::from(name)
}

/// `N` bytes from the OS CSPRNG.
///
/// Exposed because the first-run ceremony needs randomness of its own — which
/// two groups to challenge — and a second, weaker source of randomness in the
/// same feature is exactly the sort of thing that goes unnoticed.
///
/// # Errors
///
/// [`RecoveryError::Crypto`] if the operating system has no entropy source.
pub fn random_bytes<const N: usize>() -> Result<[u8; N], RecoveryError> {
    blob::os_random()
}

/// Compare two byte strings without short-circuiting on the first difference.
fn ct_eq_bytes(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b) {
        diff |= x ^ y;
    }
    std::hint::black_box(diff) == 0
}

/// Overwrite a byte buffer, resisting dead-store elimination.
fn zeroize_bytes(bytes: &mut [u8]) {
    for b in bytes.iter_mut() {
        // SAFETY: `b` is a live, uniquely-borrowed, aligned `u8`.
        unsafe { std::ptr::write_volatile(b, 0) };
    }
    std::sync::atomic::compiler_fence(std::sync::atomic::Ordering::SeqCst);
}

impl Drop for RecoveryKey {
    fn drop(&mut self) {
        zeroize_bytes(&mut self.0);
    }
}

impl Drop for MasterKeyBytes {
    fn drop(&mut self) {
        zeroize_bytes(&mut self.0);
    }
}

impl fmt::Debug for RecoveryKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("RecoveryKey(<redacted>)")
    }
}

impl fmt::Debug for MasterKeyBytes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("MasterKeyBytes(<redacted>)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn neither_key_type_can_be_printed() {
        let rk = RecoveryKey::from_bytes([0xAB; RECOVERY_KEY_BYTES]);
        let mk = MasterKeyBytes::new([0xCD; 32]);
        assert_eq!(format!("{rk:?}"), "RecoveryKey(<redacted>)");
        assert_eq!(format!("{mk:?}"), "MasterKeyBytes(<redacted>)");
        assert!(!format!("{rk:?}").contains("171"));
        assert!(!format!("{mk:?}").contains("205"));
    }

    #[test]
    fn the_blob_sits_next_to_the_database() {
        assert_eq!(
            blob_path_for(Path::new("/data/db.sqlite3")),
            PathBuf::from("/data/db.sqlite3.recovery")
        );
    }

    #[test]
    fn constant_time_compare_agrees_with_value_equality() {
        let a = RecoveryKey::from_bytes([1; RECOVERY_KEY_BYTES]);
        let b = RecoveryKey::from_bytes([1; RECOVERY_KEY_BYTES]);
        let c = RecoveryKey::from_bytes([2; RECOVERY_KEY_BYTES]);
        assert!(a.ct_eq(&b));
        assert!(!a.ct_eq(&c));

        assert!(MasterKeyBytes::new([7; 32]).ct_eq(&MasterKeyBytes::new([7; 32])));
        assert!(!MasterKeyBytes::new([7; 32]).ct_eq(&MasterKeyBytes::new([8; 32])));
    }

    #[test]
    fn a_master_key_of_the_wrong_length_is_refused_rather_than_padded() {
        assert!(MasterKeyBytes::from_slice(&[0u8; 31]).is_err());
        assert!(MasterKeyBytes::from_slice(&[0u8; 33]).is_err());
        assert!(MasterKeyBytes::from_slice(&[0u8; 32]).is_ok());
    }

    #[test]
    fn generate_does_not_return_the_same_key_twice() {
        let a = RecoveryKey::generate().unwrap();
        let b = RecoveryKey::generate().unwrap();
        assert!(!a.ct_eq(&b), "the CSPRNG returned the same key twice");
        assert!(a.expose().iter().any(|b| *b != 0), "key is all zeroes");
    }
}
